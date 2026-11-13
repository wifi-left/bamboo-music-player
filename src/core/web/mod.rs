//! Opt-in browser control panel.
//!
//! `WebControlService` owns the listener thread and a shared snapshot of what
//! the UI is showing. It never touches `AppCore` state directly: the core
//! publishes a small [`WebState`] from `tick()`, and browser actions are turned
//! into [`Command`]s that the same thread drains and executes. That keeps the
//! business rules (queue indices, readiness, persistence) in one place.
//!
//! Security model:
//! - disabled by default; only a loopback bind may run without an access key;
//! - a non-loopback bind requires a key (refused otherwise);
//! - the `Host` header must match the listener (blocks DNS rebinding);
//! - cross-origin POSTs are rejected (`Origin` check + `SameSite=Strict`);
//! - only an explicit command allow-list is exposed, never search/downloads,
//!   file paths or settings.

mod http;

use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::RwLock;
use serde::Serialize;

use crate::config::WebControlSettings;
use crate::core::app::Command;
use crate::model::RepeatMode;

pub use http::host_allowed;

const PANEL_HTML: &str = include_str!("panel.html");
const LOGIN_HTML: &str = include_str!("login.html");
const COOKIE_NAME: &str = "bamboo_web_key";
/// Publish at most five snapshots per second; a paused player publishes on
/// change only, so an idle panel costs nothing.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(200);
const MAX_CONNECTIONS: usize = 16;

/// What the browser sees for one queue row. Local file paths are deliberately
/// not exposed; the queue is addressed by index only.
#[derive(Clone, Debug, Default, Serialize)]
pub struct WebTrack {
    pub index: usize,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: Option<f64>,
    pub current: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct WebLyrics {
    pub prev: String,
    pub current: String,
    pub next: String,
    /// Translation of the current line, if the lyrics carry one.
    pub translation: String,
}

/// Snapshot served by `GET /api/state`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct WebState {
    pub version: &'static str,
    pub state: String,
    pub position: f64,
    pub duration: f64,
    pub volume: f32,
    pub muted: bool,
    pub shuffle: bool,
    pub repeat: String,
    pub track: Option<WebTrack>,
    pub lyrics: WebLyrics,
    pub queue_len: usize,
    pub current_index: Option<usize>,
    /// Bumped whenever queue rows/order change; lets the panel refetch without
    /// comparing every row.
    pub queue_rev: u64,
    pub read_only: bool,
    pub cover_rev: u64,
    pub has_cover: bool,
}

#[derive(Clone, Debug, Default)]
pub struct WebStatus {
    pub running: bool,
    /// Bound address (`ip:port`) while running.
    pub addr: Option<String>,
    /// Why the server is not running, if the last start attempt failed.
    pub error: Option<String>,
}

struct Access {
    key: String,
    read_only: bool,
}

struct Shared {
    state: RwLock<Arc<WebState>>,
    queue: RwLock<Arc<Vec<WebTrack>>>,
    cover: RwLock<Option<Arc<Vec<u8>>>>,
    access: RwLock<Access>,
    /// Last request arrival; a recently used panel keeps the UI heartbeat on.
    /// `None` means no browser has connected since startup.
    last_request: RwLock<Option<Instant>>,
    commands: Sender<Command>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

struct Running {
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
    bind: IpAddr,
    port: u16,
    join: Option<std::thread::JoinHandle<()>>,
}

pub struct WebControlService {
    shared: Arc<Shared>,
    rx: Receiver<Command>,
    running: Option<Running>,
    last_publish: Option<Instant>,
    error: Option<String>,
}

impl WebControlService {
    pub fn new(wake: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let (tx, rx) = unbounded();
        Self {
            shared: Arc::new(Shared {
                state: RwLock::new(Arc::new(WebState::default())),
                queue: RwLock::new(Arc::new(Vec::new())),
                cover: RwLock::new(None),
                access: RwLock::new(Access {
                    key: String::new(),
                    read_only: false,
                }),
                last_request: RwLock::new(None),
                commands: tx,
                wake,
            }),
            rx,
            running: None,
            last_publish: None,
            error: None,
        }
    }

    /// Apply the current settings: start, stop, restart or just update the
    /// access key/read-only flag. Returns the start error, if any.
    pub fn apply(&mut self, settings: &WebControlSettings) -> Result<(), String> {
        if !settings.enabled {
            self.stop();
            self.error = None;
            return Ok(());
        }
        if let Some(reason) = settings.startup_error() {
            self.stop();
            self.error = Some(reason.to_string());
            return Err(reason.to_string());
        }
        let bind = settings.bind_ip().expect("validated bind address");
        let key = settings.access_key.trim().to_string();
        let read_only = settings.read_only;
        if let Some(running) = self.running.as_ref()
            && running.bind == bind
            && running.port == settings.port
        {
            let mut access = self.shared.access.write();
            access.key = key;
            access.read_only = read_only;
            self.error = None;
            return Ok(());
        }
        self.stop();
        match self.start(bind, settings.port, key, read_only) {
            Ok(()) => {
                self.error = None;
                Ok(())
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    pub fn stop(&mut self) {
        let Some(mut running) = self.running.take() else {
            return;
        };
        running.stop.store(true, Ordering::SeqCst);
        // Unblock `accept()` with a throwaway loopback connection; the loop
        // checks the flag before serving it. A wildcard bind cannot be
        // connected to as 0.0.0.0, so wake it through loopback.
        let wake = if running.addr.ip().is_unspecified() {
            let ip = if running.addr.is_ipv6() {
                std::net::Ipv6Addr::LOCALHOST.into()
            } else {
                std::net::Ipv4Addr::LOCALHOST.into()
            };
            SocketAddr::new(ip, running.addr.port())
        } else {
            running.addr
        };
        let _ = TcpStream::connect_timeout(&wake, Duration::from_millis(200));
        if let Some(join) = running.join.take() {
            let _ = join.join();
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn status(&self) -> WebStatus {
        WebStatus {
            running: self.running.is_some(),
            addr: self.running.as_ref().map(|run| run.addr.to_string()),
            error: self.error.clone(),
        }
    }

    /// Commands queued by browser requests, in arrival order.
    pub fn take_commands(&self) -> Vec<Command> {
        self.rx.try_iter().collect()
    }

    pub fn should_publish(&self) -> bool {
        self.last_publish
            .is_none_or(|at| at.elapsed() >= PUBLISH_INTERVAL)
    }

    /// A browser talked to the panel recently; the UI keeps a fast heartbeat
    /// so position/lyrics stay current. Idle panels cost no periodic ticks.
    pub fn active(&self) -> bool {
        self.running.is_some()
            && self
                .shared
                .last_request
                .read()
                .is_some_and(|at| at.elapsed() < Duration::from_secs(30))
    }

    /// Push the current UI snapshot. `queue` is only rebuilt when the queue
    /// revision changes, so 10k-row queues do not get cloned five times a
    /// second.
    pub fn publish(
        &mut self,
        mut state: WebState,
        queue: Option<Vec<WebTrack>>,
        cover: Option<Arc<Vec<u8>>>,
    ) {
        if self.running.is_none() {
            return;
        }
        state.read_only = self.shared.access.read().read_only;
        *self.shared.state.write() = Arc::new(state);
        if let Some(queue) = queue {
            *self.shared.queue.write() = Arc::new(queue);
        }
        *self.shared.cover.write() = cover;
        self.last_publish = Some(Instant::now());
    }

    fn start(
        &mut self,
        bind: IpAddr,
        port: u16,
        key: String,
        read_only: bool,
    ) -> Result<(), String> {
        // A restart on the same port can race a listener that just closed;
        // retry briefly before reporting failure.
        let mut last_error = String::new();
        for attempt in 0..4 {
            match TcpListener::bind(SocketAddr::new(bind, port)) {
                Ok(listener) => {
                    let addr = listener
                        .local_addr()
                        .map_err(|err| format!("无法读取监听地址：{err}"))?;
                    {
                        let mut access = self.shared.access.write();
                        access.key = key.clone();
                        access.read_only = read_only;
                    }
                    let stop = Arc::new(AtomicBool::new(false));
                    let shared = self.shared.clone();
                    let thread_stop = stop.clone();
                    let join = std::thread::Builder::new()
                        .name("bamboo-web".to_string())
                        .spawn(move || serve(listener, shared, thread_stop, addr))
                        .map_err(|err| format!("无法创建网页控制线程：{err}"))?;
                    self.running = Some(Running {
                        stop,
                        addr,
                        bind,
                        port,
                        join: Some(join),
                    });
                    return Ok(());
                }
                Err(err) => {
                    last_error = err.to_string();
                    if attempt < 3 {
                        std::thread::sleep(Duration::from_millis(120));
                    }
                }
            }
        }
        Err(format!("无法监听 {bind}:{port}：{last_error}"))
    }
}

impl Drop for WebControlService {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Generate a 24-character URL-safe access key (~143 bits).
pub fn generate_access_key() -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    (0..24)
        .map(|_| ALPHABET[rand::random_range(0..ALPHABET.len())] as char)
        .collect()
}

fn serve(listener: TcpListener, shared: Arc<Shared>, stop: Arc<AtomicBool>, local: SocketAddr) {
    let live = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                // A wildcard bind still only serves loopback and private
                // (LAN) peers; public addresses never reach the panel.
                if !peer_allowed(peer, local.ip()) {
                    continue;
                }
                if live.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
                    live.fetch_sub(1, Ordering::AcqRel);
                    let mut stream = stream;
                    let response = http::Response::text(503, "busy");
                    let _ = http::write_response(&mut stream, &response);
                    continue;
                }
                let shared = shared.clone();
                let thread_live = live.clone();
                let spawned = std::thread::Builder::new()
                    .name("bamboo-web-conn".to_string())
                    .spawn(move || {
                        handle_connection(stream, &shared, local);
                        thread_live.fetch_sub(1, Ordering::AcqRel);
                    });
                if spawned.is_err() {
                    live.fetch_sub(1, Ordering::AcqRel);
                }
            }
            Err(err) => {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                tracing::warn!("web control accept failed: {err}");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn peer_allowed(peer: SocketAddr, local_ip: IpAddr) -> bool {
    if local_ip.is_loopback() {
        return peer.ip().is_loopback();
    }
    if local_ip.is_unspecified() {
        return peer.ip().is_loopback() || http::is_private(peer.ip());
    }
    peer.ip() == local_ip
}

fn handle_connection(mut stream: TcpStream, shared: &Shared, local: SocketAddr) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    *shared.last_request.write() = Some(Instant::now());
    // A request may arrive while the player is idle (paused, no animations,
    // no timer). Wake the UI so `poll_web` publishes a fresh snapshot before
    // the next poll instead of serving a stale one.
    if let Some(wake) = shared.wake.as_ref() {
        wake();
    }
    let response = match http::read_request(&stream) {
        Ok(request) => route(&request, shared, local),
        Err((status, message)) => json_error(status, message),
    };
    let _ = http::write_response(&mut stream, &response);
}

#[derive(Serialize)]
struct OkResponse {
    ok: bool,
}

#[derive(Serialize)]
struct ErrorResponse {
    ok: bool,
    error: &'static str,
}

#[derive(Serialize)]
struct StateResponse<'a> {
    ok: bool,
    #[serde(flatten)]
    state: &'a WebState,
}

#[derive(Serialize)]
struct QueueResponse {
    ok: bool,
    total: usize,
    offset: usize,
    rows: Vec<WebTrack>,
}

fn json_error(status: u16, message: &'static str) -> http::Response {
    http::Response::json(
        status,
        &ErrorResponse {
            ok: false,
            error: message,
        },
    )
}

fn route(request: &http::Request, shared: &Shared, local: SocketAddr) -> http::Response {
    if !http::host_allowed(request.header("host").unwrap_or(""), local) {
        return json_error(421, "无效的 Host");
    }
    let access = {
        let access = shared.access.read();
        Access {
            key: access.key.clone(),
            read_only: access.read_only,
        }
    };
    let requires_key = !access.key.is_empty();
    let authorized = !requires_key
        || request_token(request).is_some_and(|token| http::secret_eq(&token, &access.key));

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/login") => {
            if authorized {
                http::Response::redirect("/")
            } else {
                http::Response::html(200, LOGIN_HTML)
            }
        }
        ("POST", "/login") => login(request, &access),
        ("GET", "/") => {
            if authorized {
                http::Response::html(200, PANEL_HTML)
            } else {
                // The token may also arrive as `/?token=...`, which signs the
                // browser in and redirects to the clean URL.
                if let Some(token) = request.query_param("token")
                    && http::secret_eq(token, &access.key)
                {
                    let mut response = http::Response::redirect("/");
                    response
                        .headers
                        .push(("Set-Cookie".to_string(), cookie_header(&access.key)));
                    return response;
                }
                http::Response::html(200, LOGIN_HTML)
            }
        }
        ("GET", "/api/state") => {
            if !authorized {
                return json_error(401, "未授权");
            }
            let state = shared.state.read().clone();
            let response = StateResponse {
                ok: true,
                state: &state,
            };
            http::Response::json(200, &response)
        }
        ("GET", "/api/queue") => {
            if !authorized {
                return json_error(401, "未授权");
            }
            let queue = shared.queue.read().clone();
            let offset = request
                .query_param("offset")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0)
                .min(queue.len());
            let limit = request
                .query_param("limit")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(200)
                .clamp(1, 500);
            let end = offset.saturating_add(limit).min(queue.len());
            http::Response::json(
                200,
                &QueueResponse {
                    ok: true,
                    total: queue.len(),
                    offset,
                    rows: queue[offset..end].to_vec(),
                },
            )
        }
        ("GET", "/api/cover") => {
            if !authorized {
                return json_error(401, "未授权");
            }
            match shared.cover.read().as_ref() {
                Some(png) => http::Response::new(200, "image/png", png.as_slice().to_vec()),
                None => http::Response::text(404, "no cover"),
            }
        }
        ("POST", "/api/command") => {
            if !authorized {
                return json_error(401, "未授权");
            }
            if access.read_only {
                return json_error(403, "只读模式");
            }
            if let Some(origin) = request.header("origin") {
                let expected = request.header("host").map(|host| format!("http://{host}"));
                if expected.as_deref() != Some(origin) {
                    return json_error(403, "跨域请求被拒绝");
                }
            }
            let content_type = request.header("content-type").unwrap_or("");
            if !content_type
                .to_ascii_lowercase()
                .starts_with("application/json")
            {
                return json_error(415, "需要 JSON 请求体");
            }
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
                return json_error(400, "无效的 JSON");
            };
            let Some(command) = translate_command(&value) else {
                return json_error(400, "未知命令");
            };
            if shared.commands.send(command).is_err() {
                return json_error(503, "播放器未就绪");
            }
            if let Some(wake) = shared.wake.as_ref() {
                wake();
            }
            http::Response::json(200, &OkResponse { ok: true })
        }
        ("GET", "/favicon.ico") => http::Response::new(204, "image/x-icon", Vec::new()),
        _ => json_error(404, "未找到"),
    }
}

fn request_token(request: &http::Request) -> Option<String> {
    if let Some(header) = request.header("authorization")
        && let Some(token) = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
    {
        return Some(token.trim().to_string());
    }
    let cookie = request.header("cookie")?;
    let raw = http::cookie_value(cookie, COOKIE_NAME)?;
    http::percent_decode(raw, false)
}

fn cookie_header(key: &str) -> String {
    format!(
        "{COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000",
        http::percent_encode(key)
    )
}

fn login(request: &http::Request, access: &Access) -> http::Response {
    if access.key.is_empty() {
        return http::Response::redirect("/");
    }
    let form = String::from_utf8_lossy(&request.body);
    let key = form
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == "key")
        .and_then(|(_, value)| http::percent_decode(value, true))
        .unwrap_or_default();
    if !http::secret_eq(&key, &access.key) {
        // Slow down brute force attempts over the LAN.
        std::thread::sleep(Duration::from_millis(400));
        let body = LOGIN_HTML.replace(
            "<!--login-error-->",
            r#"<p class="error">访问密钥不正确，请重试。</p>"#,
        );
        return http::Response::html(401, body);
    }
    let mut response = http::Response::redirect("/");
    response
        .headers
        .push(("Set-Cookie".to_string(), cookie_header(&access.key)));
    response
}

/// Browser action → core command. Anything not listed here is not reachable
/// from the web (no queue mutation, search, downloads or settings).
fn translate_command(value: &serde_json::Value) -> Option<Command> {
    let name = value.get("command")?.as_str()?;
    let number = || value.get("value").and_then(serde_json::Value::as_f64);
    Some(match name {
        "play_pause" => Command::PlayPause,
        "next" => Command::Next,
        "prev" => Command::Prev,
        "mute" => Command::ToggleMute,
        "toggle_shuffle" => Command::ToggleShuffle,
        "cycle_repeat" => Command::CycleRepeat,
        "seek" => Command::SeekSecs(number()?.max(0.0)),
        "volume" => Command::SetVolume(number()?.clamp(0.0, 1.0) as f32),
        "play_index" => {
            let index = number()?;
            if !(index >= 0.0) {
                return None;
            }
            Command::PlayIndex(index as usize)
        }
        _ => return None,
    })
}

/// Map the engine state to the string shown by the panel.
pub fn playback_state_label(state: crate::audio::PlaybackState) -> &'static str {
    use crate::audio::PlaybackState;
    match state {
        PlaybackState::Playing => "playing",
        PlaybackState::Paused => "paused",
        PlaybackState::Loading => "loading",
        PlaybackState::Stopped => "stopped",
    }
}

pub fn repeat_label(repeat: RepeatMode) -> &'static str {
    match repeat {
        RepeatMode::Off => "off",
        RepeatMode::All => "all",
        RepeatMode::One => "one",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn generated_keys_are_long_and_url_safe() {
        let key = generate_access_key();
        assert_eq!(key.len(), 24);
        assert!(key.chars().all(|ch| ch.is_ascii_alphanumeric()));
        assert_ne!(key, generate_access_key());
    }

    #[test]
    fn only_allowed_commands_translate() {
        use serde_json::json;
        assert!(matches!(
            translate_command(&json!({"command": "play_pause"})),
            Some(Command::PlayPause)
        ));
        assert!(matches!(
            translate_command(&json!({"command": "seek", "value": 12.5})),
            Some(Command::SeekSecs(value)) if (value - 12.5).abs() < f64::EPSILON
        ));
        assert!(
            translate_command(&json!({"command": "play_index"})).is_none(),
            "a missing index must not become row 0"
        );
        assert!(translate_command(&json!({"command": "play_index", "value": -1})).is_none());
        assert!(
            translate_command(&json!({"command": "stop"})).is_none(),
            "the web panel has no stop action"
        );
        assert!(translate_command(&json!({"command": "clear_queue"})).is_none());
        assert!(translate_command(&json!({"command": "set_api_key"})).is_none());
    }

    #[test]
    fn settings_guard_lan_without_a_key() {
        let mut settings = WebControlSettings::default();
        assert!(settings.startup_error().is_none());
        settings.bind_address = "0.0.0.0".to_string();
        assert_eq!(settings.startup_error(), Some("局域网监听必须设置访问密钥"));
        settings.access_key = "123456".to_string();
        assert!(settings.startup_error().is_none());
        settings.bind_address = "not an ip".to_string();
        assert_eq!(settings.startup_error(), Some("监听地址无效"));
    }

    fn raw_request(addr: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn get(addr: SocketAddr, path: &str, cookie: Option<&str>) -> String {
        let cookie = cookie
            .map(|cookie| format!("Cookie: {COOKIE_NAME}={cookie}\r\n"))
            .unwrap_or_default();
        raw_request(
            addr,
            &format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{cookie}\r\n",
                addr.port()
            ),
        )
    }

    fn post_command(addr: SocketAddr, cookie: &str, body: &str) -> String {
        raw_request(
            addr,
            &format!(
                "POST /api/command HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {COOKIE_NAME}={cookie}\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\nOrigin: http://127.0.0.1:{}\r\n\r\n{body}",
                addr.port(),
                body.len(),
                addr.port()
            ),
        )
    }

    fn start_test_service(
        key: &str,
        read_only: bool,
    ) -> (
        WebControlService,
        SocketAddr,
        crossbeam_channel::Receiver<()>,
    ) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let wake = Arc::new(move || {
            let _ = tx.send(());
        }) as Arc<dyn Fn() + Send + Sync>;
        let mut service = WebControlService::new(Some(wake));
        let settings = WebControlSettings {
            enabled: true,
            bind_address: "127.0.0.1".to_string(),
            // Port 0 picks a free port; startup_error allows it for tests.
            port: 0,
            access_key: key.to_string(),
            read_only,
        };
        service.apply(&settings).unwrap();
        let addr = service.running.as_ref().unwrap().addr;
        (service, addr, rx)
    }

    fn sample_state() -> WebState {
        WebState {
            version: "test",
            state: "playing".to_string(),
            position: 12.5,
            duration: 200.0,
            volume: 0.6,
            muted: false,
            shuffle: true,
            repeat: "all".to_string(),
            track: Some(WebTrack {
                index: 0,
                title: "测试歌曲".to_string(),
                artist: "测试歌手".to_string(),
                album: "专辑".to_string(),
                duration: Some(200.0),
                current: true,
            }),
            lyrics: WebLyrics {
                prev: String::new(),
                current: "第一行".to_string(),
                next: "第二行".to_string(),
                translation: "第一行的翻译".to_string(),
            },
            queue_len: 1,
            current_index: Some(0),
            queue_rev: 1,
            read_only: false,
            cover_rev: 3,
            has_cover: false,
        }
    }

    #[test]
    fn panel_requires_the_key_and_serves_state_and_commands() {
        let (mut service, addr, woken) = start_test_service("test-key", false);
        let state = sample_state();
        service.publish(
            state,
            Some(vec![WebTrack {
                index: 0,
                title: "测试歌曲".to_string(),
                artist: "测试歌手".to_string(),
                album: String::new(),
                duration: Some(200.0),
                current: true,
            }]),
            None,
        );

        // Unauthenticated page load shows the key form; API calls are refused.
        let page = get(addr, "/", None);
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert!(page.contains("访问密钥"));
        let denied = get(addr, "/api/state", None);
        assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
        let authorized = get(addr, "/api/state", Some("test-key"));
        assert!(authorized.starts_with("HTTP/1.1 200"), "{authorized}");
        assert!(authorized.contains("\"title\":\"测试歌曲\""));
        assert!(authorized.contains("\"read_only\":false"));

        // Host mismatch is rejected even with a valid cookie (DNS rebinding).
        let rebinding = raw_request(
            addr,
            &format!(
                "GET /api/state HTTP/1.1\r\nHost: evil.example:{}\r\n\
                 Cookie: {COOKIE_NAME}=test-key\r\n\r\n",
                addr.port()
            ),
        );
        assert!(rebinding.starts_with("HTTP/1.1 421"), "{rebinding}");

        // A valid command reaches the core and wakes the UI.
        let response = post_command(addr, "test-key", r#"{"command":"play_pause"}"#);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(matches!(
            service.take_commands().as_slice(),
            [Command::PlayPause]
        ));
        assert!(woken.try_recv().is_ok());

        // Unknown commands and cross-origin POSTs are not forwarded.
        let unknown = post_command(addr, "test-key", r#"{"command":"clear_lyrics"}"#);
        assert!(unknown.starts_with("HTTP/1.1 400"), "{unknown}");
        let body = r#"{"command":"play_pause"}"#;
        let cross_origin = raw_request(
            addr,
            &format!(
                "POST /api/command HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {COOKIE_NAME}=test-key\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\nOrigin: http://evil.example\r\n\r\n{body}",
                addr.port(),
                body.len()
            ),
        );
        assert!(cross_origin.starts_with("HTTP/1.1 403"), "{cross_origin}");
        assert!(service.take_commands().is_empty());
        service.stop();
    }

    #[test]
    fn read_only_mode_rejects_control_but_still_serves_state() {
        let (mut service, addr, _woken) = start_test_service("test-key", true);
        service.publish(sample_state(), None, None);
        let state = get(addr, "/api/state", Some("test-key"));
        assert!(state.contains("\"read_only\":true"), "{state}");
        let response = post_command(addr, "test-key", r#"{"command":"next"}"#);
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert!(service.take_commands().is_empty());
        service.stop();
    }

    #[test]
    fn login_form_sets_the_access_cookie() {
        let (mut service, addr, _woken) = start_test_service("test-key", false);
        let body = "key=test-key";
        let response = raw_request(
            addr,
            &format!(
                "POST /login HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
                 Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
                addr.port(),
                body.len()
            ),
        );
        assert!(response.starts_with("HTTP/1.1 303"), "{response}");
        assert!(
            response.contains(&format!("{COOKIE_NAME}=test-key")),
            "{response}"
        );
        let wrong = raw_request(
            addr,
            &format!(
                "POST /login HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
                 Content-Type: application/x-www-form-urlencoded\r\nContent-Length: 9\r\n\r\nkey=wrong",
                addr.port()
            ),
        );
        assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");
        service.stop();
    }
}

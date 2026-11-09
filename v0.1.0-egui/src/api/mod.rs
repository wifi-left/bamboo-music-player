//! Bamboo-compatible HTTP API client.
//!
//! The protocol is the one implemented by `apis/local/local.php` in the
//! Bamboo music web app:
//!
//! * every action is a GET on the same endpoint with `type=<action>`;
//! * `value`, `offset`, `limit`, `br` and `prefix` are the shared parameters;
//! * search/info responses wrap their payload in `data`, and errors are
//!   frequently returned with HTTP 200 and a `code`/`msg` JSON body;
//! * `type=url` returns a plain-text playback URL (absolute or relative).
//!
//! All parsing is done on `serde_json::Value` because the IDs may be numbers
//! or strings and older servers omit optional fields.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{NetworkSettings, ProxyMode, Quality, SourceConfig};
use crate::model::{Track, TrackSource};

pub mod cache;
pub mod service;

pub use cache::SearchCache;
pub use service::{ApiEvent, ApiService, PreparedPlay};

pub type ApiResult<T> = Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    Song,
    Album,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ApiSong {
    pub id: String,
    pub artist: String,
    pub name: String,
    pub album: String,
    pub album_id: Option<String>,
    pub artist_id: Option<String>,
    pub pic: Option<String>,
    pub addition: Option<String>,
    pub release_date: Option<String>,
    pub has_mv: bool,
    pub has_audio: bool,
}

impl ApiSong {
    fn from_value(value: &Value) -> Option<Self> {
        if !value.is_object() {
            return None;
        }
        let id = value_to_string(&value["id"]).unwrap_or_default();
        if id.is_empty() {
            return None;
        }
        Some(Self {
            id,
            artist: value_to_string(&value["artist"]).unwrap_or_default(),
            name: value_to_string(&value["name"]).unwrap_or_default(),
            album: value_to_string(&value["album"]).unwrap_or_default(),
            album_id: value_to_string(&value["albumid"]).filter(|s| !s.is_empty() && s != "0"),
            artist_id: value_to_string(&value["artistid"]).filter(|s| !s.is_empty()),
            pic: value_to_string(&value["pic"]).filter(|s| !s.is_empty()),
            addition: value_to_string(&value["addition"]).filter(|s| !s.is_empty()),
            release_date: value_to_string(&value["releaseDate"]).filter(|s| !s.is_empty()),
            has_mv: truthy(&value["hasMv"]),
            has_audio: value.get("hasAudio").map(truthy).unwrap_or(true),
        })
    }

    pub fn to_track(&self, source: &SourceConfig) -> Track {
        Track {
            source: TrackSource::Bamboo {
                source_id: source.id.clone(),
                song_id: self.id.clone(),
                base_url: source.base_url.clone(),
            },
            title: if self.name.trim().is_empty() {
                self.id.clone()
            } else {
                self.name.clone()
            },
            artist: self.artist.clone(),
            album: self.album.clone(),
            album_id: self.album_id.clone(),
            artist_id: self.artist_id.clone(),
            cover_url: self.pic.clone(),
            source_name: source.name.clone(),
            ..Default::default()
        }
    }

    pub fn subtitle(&self) -> String {
        let mut out = String::new();
        if !self.artist.trim().is_empty() {
            out.push_str(&self.artist);
        }
        if !self.album.trim().is_empty() {
            if !out.is_empty() {
                out.push_str(" · ");
            }
            out.push_str(&self.album);
        }
        if let Some(addition) = self.addition.as_ref().filter(|s| !s.trim().is_empty()) {
            if !out.is_empty() {
                out.push_str(" · ");
            }
            out.push_str(addition);
        }
        out
    }
}

#[derive(Clone, Debug, Default)]
pub struct SearchPage {
    pub items: Vec<ApiSong>,
    pub total: u64,
    /// Protocol hint (`type=playlist`), currently only asserted by parsing
    /// tests; kept so the response shape stays self-describing.
    #[allow(dead_code)]
    pub playlist: bool,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SongInfo {
    pub song: ApiSong,
    pub lrc: Option<String>,
}

/// Reuse one ureq agent (and therefore its connection pool / TLS sessions)
/// for API requests with the same connection settings. Creating an agent per
/// search/cover/thumbnail request caused a fresh TCP+TLS handshake for every
/// call, which is the quickest way to overload a small Bamboo/PHP server.
static API_AGENTS: LazyLock<Mutex<HashMap<String, ureq::Agent>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const API_AGENT_CACHE_LIMIT: usize = 8;

fn agent_cache_key(http: &HttpOptions) -> String {
    let raw = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{}",
        http.user_agent,
        http.timeout_secs,
        http.connect_timeout_secs,
        http.proxy,
        http.headers,
        http.cookie,
        http.insecure_tls
    );
    blake3::hash(raw.as_bytes()).to_hex().to_string()
}

/// Connection settings shared by API calls and audio streaming. Keeping one
/// type here means the audio thread and the API worker use exactly the same
/// proxy/TLS/header configuration.
#[derive(Clone, Debug, Default)]
pub struct HttpOptions {
    pub user_agent: String,
    pub timeout_secs: u64,
    pub connect_timeout_secs: u64,
    pub proxy: ProxySetting,
    pub headers: Vec<(String, String)>,
    pub cookie: Option<String>,
    pub insecure_tls: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProxySetting {
    #[default]
    System,
    None,
    Manual(String),
}

impl HttpOptions {
    pub fn from_settings(source: &SourceConfig, network: &NetworkSettings) -> Self {
        let proxy = match network.proxy_mode {
            ProxyMode::System => ProxySetting::System,
            ProxyMode::None => ProxySetting::None,
            ProxyMode::Manual => {
                let url = network.proxy_url.trim();
                if url.is_empty() {
                    ProxySetting::System
                } else {
                    ProxySetting::Manual(url.to_string())
                }
            }
        };
        let mut headers: Vec<(String, String)> = source
            .headers
            .iter()
            .filter(|h| !h.name.trim().is_empty())
            .map(|h| (h.name.trim().to_string(), h.value.clone()))
            .collect();
        if !source.session_key.trim().is_empty() {
            headers.push((
                "X-Session-Key".to_string(),
                source.session_key.trim().to_string(),
            ));
        }
        Self {
            user_agent: if network.user_agent.trim().is_empty() {
                format!("BambooPlayer/{}", env!("CARGO_PKG_VERSION"))
            } else {
                network.user_agent.clone()
            },
            timeout_secs: network.timeout_secs.max(2),
            connect_timeout_secs: network.connect_timeout_secs.max(1),
            proxy,
            headers,
            cookie: (!source.cookie.trim().is_empty()).then(|| source.cookie.trim().to_string()),
            insecure_tls: source.insecure_tls,
        }
    }

    pub fn for_network(network: &NetworkSettings) -> Self {
        let source = SourceConfig {
            headers: Vec::new(),
            cookie: String::new(),
            session_key: String::new(),
            insecure_tls: false,
            ..SourceConfig::default()
        };
        Self::from_settings(&source, network)
    }

    /// The session key is an API credential; never forward it to a CDN or a
    /// getlocalmusic.php redirect target.
    pub fn for_stream(&self) -> Self {
        let mut options = self.clone();
        options
            .headers
            .retain(|(name, _)| !name.eq_ignore_ascii_case("x-session-key"));
        options
    }

    /// Agent for short API calls: a global deadline is desirable here.
    pub fn api_agent(&self) -> ureq::Agent {
        let mut builder = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(self.timeout_secs.max(2))))
            .timeout_connect(Some(Duration::from_secs(self.connect_timeout_secs.max(1))))
            .http_status_as_error(false)
            .max_redirects(10)
            .user_agent(self.user_agent.clone());
        builder = self.apply_proxy(builder);
        if self.insecure_tls {
            builder = builder.tls_config(
                ureq::tls::TlsConfig::builder()
                    .disable_verification(true)
                    .build(),
            );
        }
        ureq::Agent::new_with_config(builder.build())
    }

    /// Shared short-request agent. Reusing the agent keeps the connection
    /// pool alive across search, info, covers and thumbnails.
    pub fn cached_api_agent(&self) -> ureq::Agent {
        let key = agent_cache_key(self);
        if let Ok(cache) = API_AGENTS.lock()
            && let Some(agent) = cache.get(&key)
        {
            return agent.clone();
        }
        let agent = self.api_agent();
        if let Ok(mut cache) = API_AGENTS.lock() {
            if cache.len() >= API_AGENT_CACHE_LIMIT {
                cache.clear();
            }
            cache.insert(key, agent.clone());
        }
        agent
    }

    /// Agent for audio streams. A global timeout would abort long tracks, so
    /// only connect/response/read limits are applied.
    pub fn stream_agent(&self) -> ureq::Agent {
        let mut builder = ureq::Agent::config_builder()
            .timeout_global(None)
            .timeout_connect(Some(Duration::from_secs(self.connect_timeout_secs.max(1))))
            .timeout_recv_response(Some(Duration::from_secs(self.timeout_secs.max(2))))
            .timeout_per_read(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .max_redirects(10)
            .user_agent(self.user_agent.clone());
        builder = self.apply_proxy(builder);
        if self.insecure_tls {
            builder = builder.tls_config(
                ureq::tls::TlsConfig::builder()
                    .disable_verification(true)
                    .build(),
            );
        }
        ureq::Agent::new_with_config(builder.build())
    }

    fn apply_proxy(
        &self,
        builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>,
    ) -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
        match &self.proxy {
            ProxySetting::System => {
                let proxy = ureq::Proxy::try_from_env();
                builder.proxy(proxy)
            }
            ProxySetting::None => builder.proxy(None),
            ProxySetting::Manual(url) => match ureq::Proxy::new(url) {
                Ok(proxy) => builder.proxy(Some(proxy)),
                Err(err) => {
                    tracing::warn!("invalid proxy {url}: {err}");
                    builder.proxy(None)
                }
            },
        }
    }

    /// Apply configured headers to a request (used by both the API client and
    /// the streaming media source).
    pub fn apply(
        &self,
        request: ureq::RequestBuilder<ureq::typestate::WithoutBody>,
    ) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let mut request = request;
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(cookie) = &self.cookie {
            request = request.header("Cookie", cookie);
        }
        request
    }
}

#[derive(Clone, Debug)]
pub struct ApiClient {
    source: SourceConfig,
    http: HttpOptions,
    agent: ureq::Agent,
}

impl ApiClient {
    pub fn new(source: &SourceConfig, network: &NetworkSettings) -> Self {
        Self::with_http(source, HttpOptions::from_settings(source, network))
    }

    pub fn with_http(source: &SourceConfig, http: HttpOptions) -> Self {
        let agent = http.cached_api_agent();
        Self {
            source: source.clone(),
            http,
            agent,
        }
    }

    fn request_text(
        &self,
        action: &str,
        value: &str,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> ApiResult<String> {
        self.request_text_with_seed(action, value, offset, limit, None)
    }

    fn request_text_with_seed(
        &self,
        action: &str,
        value: &str,
        offset: Option<u32>,
        limit: Option<u32>,
        seed: Option<u64>,
    ) -> ApiResult<String> {
        if self.source.base_url.trim().is_empty() {
            return Err(
                crate::i18n::tr("音源地址为空，请先在设置中填写 Bamboo API 地址").to_string(),
            );
        }
        let endpoint = self.source.endpoint_url(action, None);
        if endpoint.trim().is_empty() {
            return Err(crate::i18n::tr("音源端点为空").to_string());
        }
        let attempts = 1 + self.source_retries();
        let mut last_error = String::new();
        for attempt in 0..attempts {
            let mut request = self.agent.get(&endpoint).query("type", action);
            if !value.is_empty() {
                request = request.query("value", value);
            }
            if let Some(offset) = offset {
                request = request.query("offset", offset.to_string());
            }
            if let Some(limit) = limit {
                request = request.query("limit", limit.to_string());
            }
            if let Some(seed) = seed {
                request = request.query("seed", seed.to_string());
                request = request.query("offset", seed.to_string());
            }
            request = request.query("br", self.source.quality.api_value());
            if !self.source.prefix.trim().is_empty() {
                request = request.query("prefix", self.source.prefix.trim());
            }
            let request = self.http.apply(request);
            match request.call() {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    let text = response
                        .body_mut()
                        .with_config()
                        .limit(16 * 1024 * 1024)
                        .read_to_string()
                        .map_err(|err| crate::trf!("读取音源响应失败: {}", err))?;
                    if status >= 500 && attempt + 1 < attempts {
                        last_error = crate::trf!("音源服务错误 HTTP {}", status);
                        std::thread::sleep(Duration::from_millis(250 * (attempt as u64 + 1)));
                        continue;
                    }
                    if status >= 400 {
                        return Err(crate::trf!(
                            "音源请求失败 HTTP {}: {}",
                            status,
                            truncate(&text, 160)
                        ));
                    }
                    return Ok(text.trim_start_matches('\u{feff}').to_string());
                }
                Err(err) => {
                    last_error = crate::trf!("无法连接音源: {}", err);
                    if attempt + 1 < attempts {
                        std::thread::sleep(Duration::from_millis(250 * (attempt as u64 + 1)));
                    }
                }
            }
        }
        Err(last_error)
    }

    fn source_retries(&self) -> u32 {
        // HttpOptions does not carry the retry count; requests here are cheap
        // and a single retry is enough to hide transient socket errors.
        1
    }

    pub fn search(&self, keyword: &str, offset: u32, limit: u32) -> ApiResult<SearchPage> {
        let body = self.request_text("search", keyword, Some(offset), Some(limit))?;
        parse_search(&body, &self.source.base_url)
    }

    pub fn search_albums(&self, keyword: &str, offset: u32, limit: u32) -> ApiResult<SearchPage> {
        let body = self.request_text("searchAlbum", keyword, Some(offset), Some(limit))?;
        parse_search(&body, &self.source.base_url)
    }

    pub fn album(&self, album_id: &str, offset: u32, limit: u32) -> ApiResult<SearchPage> {
        let body = self.request_text("album", album_id, Some(offset), Some(limit))?;
        parse_search(&body, &self.source.base_url)
    }

    pub fn artist(&self, artist_id: &str, offset: u32, limit: u32) -> ApiResult<SearchPage> {
        let body = self.request_text("singer", artist_id, Some(offset), Some(limit))?;
        parse_search(&body, &self.source.base_url)
    }

    pub fn random_seeded(&self, seed: u64, count: u32) -> ApiResult<SearchPage> {
        let body =
            self.request_text_with_seed("random", &count.to_string(), None, None, Some(seed))?;
        parse_search(&body, &self.source.base_url)
    }

    pub fn suggest(&self, keyword: &str) -> ApiResult<Vec<String>> {
        let body = self.request_text("suggestKey", keyword, None, None)?;
        let value: Value = serde_json::from_str(&body)
            .map_err(|err| crate::trf!("音源返回了无效 JSON: {}", err))?;
        let mut out = Vec::new();
        if let Some(items) = value["data"].as_array() {
            for item in items {
                if let Some(text) = value_to_string(item) {
                    if !text.trim().is_empty() {
                        out.push(text);
                    }
                }
            }
        }
        Ok(out)
    }

    pub fn info(&self, song_id: &str) -> ApiResult<SongInfo> {
        let body = self.request_text("info", song_id, None, None)?;
        parse_info(&body, &self.source.base_url)
    }

    pub fn song_url(&self, song_id: &str) -> ApiResult<String> {
        let body = self.request_text("url", song_id, None, None)?;
        let body = body.trim();
        if body.is_empty() {
            return Err(crate::i18n::tr("音源没有返回播放地址").to_string());
        }
        if body.starts_with('{') {
            let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            let msg = value_to_string(&value["msg"])
                .or_else(|| value_to_string(&value["message"]))
                .unwrap_or_else(|| truncate(body, 160));
            return Err(msg);
        }
        Ok(absolute_url(&self.source.base_url, body))
    }

    pub fn status(&self) -> ApiResult<String> {
        let body = self.request_text("status", "", None, None)?;
        if let Ok(value) = serde_json::from_str::<Value>(&body) {
            if let Some(total) = value["total_songs"].as_u64() {
                return Ok(crate::trf!("连接成功 · {} 首", total));
            }
            if value["code"].as_u64() == Some(200) {
                return Ok(crate::i18n::tr("连接成功").to_string());
            }
            if let Some(msg) = value_to_string(&value["msg"]) {
                return Err(msg);
            }
        }
        Ok(crate::i18n::tr("连接成功（未返回状态信息）").to_string())
    }

    /// Resolve everything needed to start an online track in one worker job.
    /// `info` is best-effort: playback proceeds even when the metadata or
    /// lyrics endpoint fails.
    pub fn prepare(&self, song_id: &str, quality: Quality) -> ApiResult<PreparedPlay> {
        let info = self.info(song_id).ok();
        let url = if self.source.quality == quality {
            self.song_url(song_id)?
        } else {
            let mut source = self.source.clone();
            source.quality = quality;
            ApiClient::with_http(&source, self.http.clone()).song_url(song_id)?
        };
        Ok(PreparedPlay {
            url,
            info: info.as_ref().map(|i| i.song.clone()),
            lrc: info.and_then(|i| i.lrc),
        })
    }
}

fn parse_search(body: &str, base_url: &str) -> ApiResult<SearchPage> {
    let value: Value =
        serde_json::from_str(body).map_err(|err| crate::trf!("音源返回了无效 JSON: {}", err))?;
    if value.get("data").is_none() {
        let code = value["code"].as_u64().unwrap_or(0);
        let msg = value_to_string(&value["msg"])
            .or_else(|| value_to_string(&value["message"]))
            .unwrap_or_else(|| truncate(body, 160));
        return Err(if code != 0 {
            crate::trf!("音源错误 {}: {}", code, msg)
        } else {
            msg
        });
    }
    let data = &value["data"];
    let items_value = data["list"].as_array().cloned().unwrap_or_default();
    // Album/playlist search results only carry `id` (the album id); the song
    // rows use `albumid`. Fill the gap so the UI can open these entries.
    let playlist = value_to_string(&value["type"]).as_deref() == Some("playlist");
    let mut items = Vec::with_capacity(items_value.len());
    for item in &items_value {
        if let Some(mut song) = ApiSong::from_value(item) {
            if let Some(pic) = song.pic.as_mut() {
                *pic = absolute_url(base_url, pic);
            }
            if playlist && song.album_id.is_none() {
                song.album_id = Some(song.id.clone());
            }
            items.push(song);
        }
    }
    let total = value_to_u64(&data["total"])
        .or_else(|| value_to_u64(&value["total"]))
        .unwrap_or(items.len() as u64);
    let name = value_to_string(&data["name"]).or_else(|| value_to_string(&value["name"]));
    Ok(SearchPage {
        items,
        total,
        playlist,
        name,
    })
}

fn parse_info(body: &str, base_url: &str) -> ApiResult<SongInfo> {
    let value: Value =
        serde_json::from_str(body).map_err(|err| crate::trf!("音源返回了无效 JSON: {}", err))?;
    let info = &value["data"]["info"];
    let Some(mut song) = ApiSong::from_value(info) else {
        let msg = value_to_string(&value["msg"])
            .or_else(|| value_to_string(&value["message"]))
            .unwrap_or_else(|| crate::i18n::tr("音源没有返回歌曲信息").to_string());
        return Err(msg);
    };
    if let Some(pic) = song.pic.as_mut() {
        *pic = absolute_url(base_url, pic);
    }
    let lrc = value_to_string(&value["data"]["lrc"]).filter(|s| !s.trim().is_empty());
    Ok(SongInfo { song, lrc })
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn value_to_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_u64().map(|v| v != 0).unwrap_or(false),
        Value::String(s) => matches!(s.trim(), "1" | "true" | "yes"),
        _ => false,
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// Resolve the server-relative picture/playback links used by the PHP API.
fn absolute_url(base: &str, value: &str) -> String {
    let value = value.trim();
    if value.starts_with("http://") || value.starts_with("https://") {
        return value.to_string();
    }
    let Ok(base_url) = url::Url::parse(base) else {
        return value.to_string();
    };
    if let Some(rest) = value.strip_prefix("//") {
        return format!("{}://{}", base_url.scheme(), rest);
    }
    if value.starts_with('/') {
        return format!(
            "{}://{}{}",
            base_url.scheme(),
            base_url
                .host_str()
                .map(|h| match base_url.port() {
                    Some(p) => format!("{h}:{p}"),
                    None => h.to_string(),
                })
                .unwrap_or_default(),
            value
        );
    }
    // The API lives at `<site>/apis/<name>/<script>.php`. A value such as
    // `./apis/local/cover.php` is relative to the site root, not the API
    // directory, so strip the two API path segments before joining.
    if let Some(rest) = value.strip_prefix("./") {
        let mut segments: Vec<&str> = base_url
            .path_segments()
            .map(|s| s.collect())
            .unwrap_or_default();
        // Drop the script file and the source directory (`local`, `onlinea`, ...).
        if !segments.is_empty() {
            segments.pop();
        }
        if segments
            .last()
            .map(|s| *s == "local" || *s == "video" || s.starts_with("online"))
            .unwrap_or(false)
        {
            segments.pop();
        }
        if segments.last() == Some(&"apis") {
            segments.pop();
        }
        let root = segments.join("/");
        let path = if rest.starts_with("apis/") {
            rest.to_string()
        } else {
            format!("apis/{rest}")
        };
        return format!(
            "{}://{}/{}/{}",
            base_url.scheme(),
            base_url.host_str().unwrap_or_default(),
            root.trim_matches('/'),
            path.trim_start_matches('/')
        );
    }
    base_url
        .join(value)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_cache_key_changes_with_connection_settings() {
        let mut a = HttpOptions::default();
        let first = agent_cache_key(&a);
        assert_eq!(first, agent_cache_key(&a));
        a.timeout_secs += 1;
        assert_ne!(first, agent_cache_key(&a));
        let mut b = HttpOptions::default();
        b.headers
            .push(("X-Session-Key".to_string(), "secret".to_string()));
        assert_ne!(first, agent_cache_key(&b));
    }

    #[test]
    fn parses_song_search_with_numeric_ids() {
        let body = r#"{"data":{"total":31,"list":[
            {"id":1898,"addition":"","artist":"莫文蔚","name":"外面的世界","album":"其他音乐",
             "albumid":1592,"pic":"./apis/local/cover.php?id=784","artistid":"6I6r5paH6JSa","releaseDate":null}
        ]}}"#;
        let page = parse_search(body, "http://127.0.0.1/musicnew/apis/local/local.php").unwrap();
        assert_eq!(page.total, 31);
        assert!(!page.playlist);
        assert_eq!(page.items[0].id, "1898");
        assert_eq!(page.items[0].name, "外面的世界");
        assert_eq!(
            page.items[0].pic.as_deref(),
            Some("http://127.0.0.1/musicnew/apis/local/cover.php?id=784")
        );
    }

    #[test]
    fn parses_album_playlist() {
        let body = r#"{"data":{"total":21,"list":[
            {"name":"番剧 偶像大师","id":783,"pic":"./apis/local/cover.php?id=784"}
        ]},"type":"playlist"}"#;
        let page = parse_search(body, "http://127.0.0.1/musicnew/apis/local/local.php").unwrap();
        assert!(page.playlist);
        assert_eq!(page.items[0].id, "783");
        assert_eq!(page.items[0].album_id.as_deref(), Some("783"));
        assert!(page.items[0].name.contains("偶像大师"));
    }

    #[test]
    fn parses_info_and_returns_error_json() {
        let body = r#"{"data":{"info":{"id":808,"artist":"A","name":"N","album":"B"},
            "lrc":"[00:01.00]hello"}}"#;
        let info = parse_info(body, "http://h/apis/local/local.php").unwrap();
        assert_eq!(info.song.artist, "A");
        assert!(info.lrc.unwrap().contains("hello"));

        let err =
            parse_search(r#"{"code":404,"msg":"404 - 此歌曲不存在"}"#, "http://h/").unwrap_err();
        assert!(err.contains("404"));
    }

    #[test]
    fn resolves_relative_links() {
        let base = "http://127.0.0.1/musicnew/apis/local/local.php";
        assert_eq!(
            absolute_url(base, "./apis/local/cover.php?id=784"),
            "http://127.0.0.1/musicnew/apis/local/cover.php?id=784"
        );
        assert_eq!(
            absolute_url(base, "getlocalmusic.php?id=1"),
            "http://127.0.0.1/musicnew/apis/local/getlocalmusic.php?id=1"
        );
        assert_eq!(
            absolute_url(base, "/musicnew/a.mp3"),
            "http://127.0.0.1/musicnew/a.mp3"
        );
    }

    #[test]
    fn track_keeps_stable_bamboo_key() {
        let source = SourceConfig::preset(crate::config::SourceKind::Online);
        let song = ApiSong {
            id: "42".into(),
            name: "Title".into(),
            artist: "Artist".into(),
            ..Default::default()
        };
        let track = song.to_track(&source);
        assert!(track.dedupe_key().ends_with(":42"));
        assert_eq!(track.display_title(), "Title");
    }

    /// Optional live check against the local Bamboo web app. Run with:
    /// `cargo test live_local_bamboo -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_local_random_station() {
        let source = SourceConfig::preset(crate::config::SourceKind::Local);
        let network = NetworkSettings::default();
        let client = ApiClient::new(&source, &network);
        let first = client.random_seeded(1234, 6).expect("seeded random");
        let second = client.random_seeded(5678, 6).expect("seeded random");
        assert!(!first.items.is_empty());
        assert!(!second.items.is_empty());
        assert!(first.items.iter().all(|song| !song.id.trim().is_empty()));
        eprintln!(
            "station batch: {} / {} songs, first={:?}",
            first.items.len(),
            second.items.len(),
            first.items[0].name
        );
    }

    #[test]
    #[ignore]
    fn live_local_bamboo() {
        let source = SourceConfig::preset(crate::config::SourceKind::Local);
        let network = NetworkSettings::default();
        let client = ApiClient::new(&source, &network);
        let status = client.status().expect("status");
        eprintln!("status: {status}");

        let page = client.search("莫文蔚", 1, 5).expect("search");
        assert!(!page.items.is_empty());
        eprintln!(
            "search: {} items, first={:?} / {:?}",
            page.items.len(),
            page.items[0].artist,
            page.items[0].name
        );
        let song = page.items[0].clone();
        let info = client.info(&song.id).expect("info");
        eprintln!(
            "info: {} - {}, lrc={}",
            info.song.artist,
            info.song.name,
            info.lrc.as_deref().map(|s| s.len()).unwrap_or(0)
        );
        let url = client.song_url(&song.id).expect("url");
        eprintln!("url: {url}");
        assert!(url.starts_with("http"));

        let suggestions = client.suggest("莫").expect("suggest");
        assert!(!suggestions.is_empty());
        eprintln!("suggest: {suggestions:?}");

        let artist_id = song.artist_id.clone().expect("artist id");
        let artist = client.artist(&artist_id, 1, 5).expect("artist");
        assert!(!artist.items.is_empty());
        eprintln!(
            "artist {artist_id}: total={}, first={:?}",
            artist.total, artist.items[0].name
        );
    }
}

//! HTTP media source for symphonia with Range seeks and a small disk cache.
//!
//! The decoder thread owns this reader, so blocking network reads are fine.
//! `Range: bytes=<pos>-` makes seeks cheap on servers that support it; when a
//! server ignores Range (HTTP 200) the reader skips forward instead. Reads are
//! mirrored into a `.part` file only while they are strictly sequential from
//! byte zero, and the file is published as the final cache entry once EOF is
//! reached. A partial or randomly-seeked download is discarded on drop.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use symphonia::core::io::MediaSource;

use crate::api::HttpOptions;

const READ_RETRIES: u8 = 2;

pub struct HttpMediaSource {
    agent: ureq::Agent,
    url: String,
    http: HttpOptions,
    reader: Mutex<Box<dyn Read + Send>>,
    pos: u64,
    len: Option<u64>,
    seekable: bool,
    retries_left: u8,
    cache: Option<CacheWriter>,
    /// Response `Content-Type`, used to detect an HLS playlist served from a
    /// URL without a `.m3u8` suffix.
    content_type: Option<String>,
    /// Bytes fetched while probing for `#EXTM3U`; replayed before the live
    /// reader so the probe is invisible to the decoder.
    prefix: Vec<u8>,
    prefix_pos: usize,
}

impl HttpMediaSource {
    pub fn open(url: String, http: HttpOptions, cache_path: Option<PathBuf>) -> io::Result<Self> {
        let agent = http.stream_agent();
        let cache = cache_path.map(CacheWriter::new);
        let mut source = Self {
            agent,
            url,
            http,
            reader: Mutex::new(Box::new(io::empty())),
            pos: 0,
            len: None,
            seekable: false,
            retries_left: READ_RETRIES,
            cache,
            content_type: None,
            prefix: Vec::new(),
            prefix_pos: 0,
        };
        source.open_at(0)?;
        Ok(source)
    }

    /// Open and peek at the first bytes. Returns `(source, is_hls)`; a true
    /// result means the caller should discard this reader and use
    /// `HlsMediaSource` instead. Servers that omit the `.m3u8` extension or
    /// serve a playlist through a generic download endpoint are covered here.
    pub fn open_probed(
        url: String,
        http: HttpOptions,
        cache_path: Option<PathBuf>,
    ) -> io::Result<(Self, bool)> {
        let mut source = Self::open(url, http, cache_path)?;
        let mut head = [0u8; 64];
        let read = source.read(&mut head)?;
        if source.is_hls_playlist() || head[..read].starts_with(b"#EXTM3U") {
            return Ok((source, true));
        }
        source.prefix = head[..read].to_vec();
        source.prefix_pos = 0;
        source.pos = 0;
        Ok((source, false))
    }

    pub fn is_hls_playlist(&self) -> bool {
        self.content_type
            .as_deref()
            .is_some_and(|value| {
                let value = value.to_ascii_lowercase();
                value.contains("mpegurl") || value.contains("vnd.apple.mpeg")
            })
            || self.prefix.starts_with(b"#EXTM3U")
    }

    fn open_at(&mut self, offset: u64) -> io::Result<()> {
        // Drop the previous connection before asking for a new range. Some
        // local PHP/IIS setups only serve one request at a time; keeping the
        // old reader alive while opening the new one made every seek wait for
        // itself until the request timed out.
        *self.reader.get_mut().unwrap() = Box::new(io::empty());
        self.prefix.clear();
        self.prefix_pos = 0;
        let requested = if offset > 0 { Some(offset) } else { None };
        let response = self.request(requested)?;
        let status = response.status().as_u16();
        if status == 416 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                crate::i18n::tr("服务器拒绝了断点续传请求"),
            ));
        }
        if status >= 400 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("HTTP {status}"),
            ));
        }

        let content_range = response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        self.content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_ascii_lowercase());

        let mut start = 0u64;
        let mut total = None;
        if status == 206 {
            if let Some(range) = content_range.as_deref() {
                let (s, t) = parse_content_range(range)?;
                start = s;
                total = t;
            }
        } else {
            total = content_length;
        }

        let body = response.into_body();
        let mut reader: Box<dyn Read + Send> = Box::new(body.into_reader());
        if start < offset {
            // A 206 response may start slightly before the requested byte.
            discard(&mut reader, offset - start)?;
            start = offset;
        } else if start > offset {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                crate::i18n::tr("服务器返回了错误的 Range 起点"),
            ));
        }

        self.len = total;
        self.pos = start;
        self.seekable = total.is_some();
        self.retries_left = READ_RETRIES;
        *self.reader.get_mut().unwrap() = reader;
        Ok(())
    }

    fn request(&self, offset: Option<u64>) -> io::Result<ureq::http::Response<ureq::Body>> {
        let mut request = self.http.apply(self.agent.get(&self.url));
        request = request.header("Accept-Encoding", "identity");
        if let Some(offset) = offset {
            request = request.header("Range", format!("bytes={offset}-"));
        }
        request.call().map_err(|err| {
            io::Error::new(
                io::ErrorKind::Other,
                crate::trf!("在线音频请求失败: {}", err),
            )
        })
    }

    fn read_retry(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let result = {
            let mut reader = self.reader.lock().unwrap();
            reader.read(buf)
        };
        match result {
            Ok(0) => {
                if let Some(cache) = self.cache.as_mut() {
                    cache.finish(self.len);
                }
                Ok(0)
            }
            Ok(n) => {
                self.pos = self.pos.saturating_add(n as u64);
                if let Some(cache) = self.cache.as_mut() {
                    cache.write_at(self.pos - n as u64, &buf[..n]);
                }
                Ok(n)
            }
            Err(err) => {
                if self.retries_left > 0 && self.pos > 0 {
                    self.retries_left -= 1;
                    tracing::warn!("stream read failed at {}: {err}; reconnecting", self.pos);
                    if self.open_at(self.pos).is_ok() {
                        return self.read_retry(buf);
                    }
                }
                Err(err)
            }
        }
    }
}

impl Read for HttpMediaSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.prefix_pos < self.prefix.len() {
            let available = self.prefix.len() - self.prefix_pos;
            let count = available.min(buf.len());
            buf[..count]
                .copy_from_slice(&self.prefix[self.prefix_pos..self.prefix_pos + count]);
            self.prefix_pos += count;
            self.pos = self.pos.saturating_add(count as u64);
            if self.prefix_pos == self.prefix.len() {
                self.prefix.clear();
                self.prefix_pos = 0;
            }
            return Ok(count);
        }
        self.read_retry(buf)
    }
}

impl Seek for HttpMediaSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(v) => v,
            SeekFrom::Current(delta) => {
                if delta >= 0 {
                    self.pos.saturating_add(delta as u64)
                } else {
                    self.pos.saturating_sub((-delta) as u64)
                }
            }
            SeekFrom::End(delta) => {
                let len = self.len.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Unsupported, crate::i18n::tr("长度未知"))
                })?;
                if delta >= 0 {
                    len.saturating_add(delta as u64)
                } else {
                    len.saturating_sub((-delta) as u64)
                }
            }
        };
        if target == self.pos {
            return Ok(self.pos);
        }
        if target < self.prefix.len() as u64 {
            self.pos = target;
            self.prefix_pos = target as usize;
            return Ok(target);
        }
        // Keep the cache writer: it only mirrors strictly sequential reads, so
        // seeks are ignored rather than invalidating an already-built prefix.
        self.open_at(target)?;
        Ok(self.pos)
    }
}

impl MediaSource for HttpMediaSource {
    fn is_seekable(&self) -> bool {
        self.seekable
    }

    fn byte_len(&self) -> Option<u64> {
        self.len
    }
}

fn parse_content_range(value: &str) -> io::Result<(u64, Option<u64>)> {
    // bytes 100-199/1234
    let value = value.trim();
    let rest = value.strip_prefix("bytes ").unwrap_or(value);
    let (range, total) = rest.split_once('/').unwrap_or((rest, ""));
    let (start, _end) = range.split_once('-').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            crate::i18n::tr("无效的 Content-Range"),
        )
    })?;
    let start = start.trim().parse::<u64>().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            crate::i18n::tr("无效的 Range 起点"),
        )
    })?;
    let total = total.trim();
    let total = if total == "*" || total.is_empty() {
        None
    } else {
        total.parse::<u64>().ok()
    };
    Ok((start, total))
}

fn discard(reader: &mut Box<dyn Read + Send>, mut bytes: u64) -> io::Result<()> {
    let mut scratch = [0u8; 64 * 1024];
    while bytes > 0 {
        let want = bytes.min(scratch.len() as u64) as usize;
        let n = reader.read(&mut scratch[..want])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                crate::i18n::tr("服务器提前结束了响应"),
            ));
        }
        bytes -= n as u64;
    }
    Ok(())
}

struct CacheWriter {
    part: PathBuf,
    final_path: PathBuf,
    file: Option<File>,
    next: u64,
    done: bool,
}

impl CacheWriter {
    fn new(final_path: PathBuf) -> Self {
        let part = final_path.with_extension("part");
        if let Some(parent) = final_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::remove_file(&part);
        Self {
            part,
            final_path,
            file: None,
            next: 0,
            done: false,
        }
    }

    fn write_at(&mut self, pos: u64, data: &[u8]) {
        if self.done || pos != self.next {
            return;
        }
        if self.file.is_none() {
            match OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&self.part)
            {
                Ok(file) => self.file = Some(file),
                Err(err) => {
                    tracing::warn!("cache create failed: {err}");
                    self.done = true;
                    return;
                }
            }
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if file.seek(SeekFrom::Start(pos)).is_err() || file.write_all(data).is_err() {
            self.done = true;
            return;
        }
        self.next += data.len() as u64;
    }

    fn finish(&mut self, expected: Option<u64>) {
        if self.done {
            return;
        }
        self.done = true;
        let Some(file) = self.file.take() else {
            let _ = std::fs::remove_file(&self.part);
            return;
        };
        let _ = file.sync_all();
        drop(file);
        let complete = expected
            .map(|len| len > 0 && self.next == len)
            .unwrap_or(false);
        if complete {
            if std::fs::rename(&self.part, &self.final_path).is_err() {
                let _ = std::fs::remove_file(&self.final_path);
                let _ = std::fs::rename(&self.part, &self.final_path);
            }
            tracing::info!("stream cache complete: {}", self.final_path.display());
        } else {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

impl Drop for CacheWriter {
    fn drop(&mut self) {
        if !self.done {
            self.file = None;
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    /// Serve exactly `requests` connections, one at a time. Each connection is
    /// held open until the client closes it, emulating a PHP/IIS worker that
    /// cannot accept the next request while a stream is still attached.
    fn single_connection_server(listener: TcpListener, requests: usize) {
        const TOTAL: u64 = 1_000_000;
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().expect("accept");
            stream.set_nodelay(true).ok();
            let mut buf = [0u8; 4096];
            let read = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..read]).to_ascii_lowercase();
            let range_start = request.lines().find_map(|line| {
                line.strip_prefix("range: bytes=")
                    .and_then(|value| value.split('-').next())
                    .and_then(|value| value.trim().parse::<u64>().ok())
            });
            let header = match range_start {
                Some(start) => format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{}/{} \r\nAccept-Ranges: bytes\r\nContent-Type: audio/mpeg\r\nConnection: close\r\n\r\n",
                    TOTAL - start,
                    TOTAL - 1,
                    TOTAL
                ),
                None => format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {TOTAL}\r\nAccept-Ranges: bytes\r\nContent-Type: audio/mpeg\r\nConnection: close\r\n\r\n"
                ),
            };
            stream.write_all(header.as_bytes()).ok();
            // A short prefix of the body is enough; the client is expected to
            // drop the reader before asking for a range.
            stream.write_all(&[0u8; 512]).ok();
            stream.flush().ok();
            stream.set_nonblocking(true).ok();
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                match stream.peek(&mut [0u8; 1]) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn seek_closes_the_old_connection_before_requesting_a_range() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("server address");
        let server = std::thread::spawn(move || single_connection_server(listener, 2));

        let http = HttpOptions {
            timeout_secs: 2,
            connect_timeout_secs: 2,
            proxy: crate::api::ProxySetting::None,
            user_agent: "BambooTest/1.0".to_string(),
            ..Default::default()
        };
        let mut source = HttpMediaSource::open(format!("http://{address}/stream"), http, None)
            .expect("open source");
        let position = source
            .seek(SeekFrom::Start(100))
            .expect("seek must not wait for the old stream");
        assert_eq!(position, 100);
        drop(source);
        server.join().expect("server thread");
    }

    #[test]
    fn parses_content_range_variants() {
        assert_eq!(
            parse_content_range("bytes 100-199/1234").unwrap(),
            (100, Some(1234))
        );
        assert_eq!(parse_content_range("bytes 0-0/*").unwrap(), (0, None));
        assert!(parse_content_range("nonsense").is_err());
    }

    fn one_shot_server(content_type: &str, body: &'static [u8]) -> String {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let content_type = content_type.to_string();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).unwrap();
            socket.write_all(body).unwrap();
        });
        format!("http://{address}/stream")
    }

    #[test]
    fn probes_hls_playlists_served_without_an_m3u8_extension() {
        let url = one_shot_server(
            "application/vnd.apple.mpegurl",
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.aac\n#EXT-X-ENDLIST\n",
        );
        let http = HttpOptions {
            timeout_secs: 2,
            connect_timeout_secs: 2,
            proxy: crate::api::ProxySetting::None,
            ..Default::default()
        };
        let (source, is_hls) = HttpMediaSource::open_probed(url, http, None).unwrap();
        assert!(is_hls, "playlist content type must select the HLS path");
        assert!(source.is_hls_playlist());
    }

    #[test]
    fn probe_prefix_is_replayed_without_losing_bytes() {
        let url = one_shot_server("audio/mpeg", b"ID3\x04\x00\x00\x00\x00\x00\x00hello-world");
        let http = HttpOptions {
            timeout_secs: 2,
            connect_timeout_secs: 2,
            proxy: crate::api::ProxySetting::None,
            ..Default::default()
        };
        let (mut source, is_hls) = HttpMediaSource::open_probed(url, http, None).unwrap();
        assert!(!is_hls);
        let mut out = Vec::new();
        source.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"ID3\x04\x00\x00\x00\x00\x00\x00hello-world");
    }
}

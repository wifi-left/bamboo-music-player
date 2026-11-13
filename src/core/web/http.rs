//! Minimal HTTP/1.1 helpers for the web control panel.
//!
//! Only what the panel needs: one request per connection (`Connection: close`),
//! bounded header/body sizes, absolute-form-free request targets and the
//! percent-decoding used by paths, query strings and cookies. Nothing here
//! touches playback state, so the parsers are unit-testable on their own.

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};

/// Largest accepted request head (request line + headers).
pub const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Largest accepted body; commands are tiny JSON objects.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

pub struct Request {
    pub method: String,
    /// Percent-decoded path, always starting with `/`.
    pub path: String,
    pub query: Vec<(String, String)>,
    /// Header names are lowercased; values keep their original spelling.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Rejection status used when a request cannot be parsed.
pub type ReadError = (u16, &'static str);

pub fn read_request(mut stream: &TcpStream) -> Result<Request, ReadError> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    let head_end = loop {
        if let Some(end) = find_head_end(&buf) {
            break end;
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err((431, "请求头过大"));
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                if buf.is_empty() {
                    return Err((400, "空请求"));
                }
                return Err((400, "请求被截断"));
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                return Err((408, "读取超时"));
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {
                return Err((408, "读取超时"));
            }
            Err(_) => return Err((400, "读取失败")),
        }
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let target = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    if method.is_empty() || !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Err((400, "无效的请求行"));
    }

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err((400, "无效的请求头"));
        };
        let name = name.trim();
        if name.is_empty() {
            return Err((400, "无效的请求头"));
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    if headers.iter().any(|(name, _)| name == "transfer-encoding") {
        return Err((411, "不支持分块请求"));
    }
    let content_length = match headers.iter().find(|(name, _)| name == "content-length") {
        Some((_, value)) => value
            .parse::<usize>()
            .map_err(|_| (400, "无效的 Content-Length"))?,
        None => 0,
    };
    if content_length > MAX_BODY_BYTES {
        return Err((413, "请求体过大"));
    }

    let body_start = head_end + 4;
    while buf.len() < body_start + content_length {
        match stream.read(&mut chunk) {
            Ok(0) => return Err((400, "请求体被截断")),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return Err((400, "读取请求体失败")),
        }
        if buf.len() > body_start + MAX_BODY_BYTES + MAX_HEAD_BYTES {
            return Err((413, "请求体过大"));
        }
    }

    let (raw_path, raw_query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    };
    let path = percent_decode(raw_path, false).ok_or((400, "无效的路径编码"))?;
    if !path.starts_with('/') {
        return Err((400, "无效的请求路径"));
    }
    let mut query = Vec::new();
    for pair in raw_query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = match pair.split_once('=') {
            Some((key, value)) => (key, value),
            None => (pair, ""),
        };
        let key = percent_decode(key, true).ok_or((400, "无效的查询编码"))?;
        let value = percent_decode(value, true).ok_or((400, "无效的查询编码"))?;
        query.push((key, value));
    }

    Ok(Request {
        method,
        path,
        query,
        headers,
        body: buf[body_start..body_start + content_length].to_vec(),
    })
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Decode `%XX` escapes; `plus` additionally maps `+` to a space (form/query
/// encoding), which is not done for URL path segments.
pub fn percent_decode(input: &str, plus: bool) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let high = *bytes.get(index + 1)?;
                let low = *bytes.get(index + 2)?;
                out.push((hex(high)? << 4) | hex(low)?);
                index += 3;
            }
            b'+' if plus => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Percent-encode everything outside the unreserved set, for cookie values.
pub fn percent_encode(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(*byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0F) as usize] as char);
        }
    }
    out
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Constant-time-ish secret comparison (length is still observable).
pub fn secret_eq(candidate: &str, expected: &str) -> bool {
    let candidate = candidate.as_bytes();
    let expected = expected.as_bytes();
    if candidate.len() != expected.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in candidate.iter().zip(expected) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Value of one cookie from a `Cookie:` header.
pub fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    for part in header.split(';') {
        let part = part.trim();
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        if key.trim() == name {
            return Some(value.trim());
        }
    }
    None
}

/// Validate the `Host` header against the listening socket.
///
/// Rejecting unknown hosts blocks DNS-rebinding pages from talking to the
/// panel: an attacker-controlled domain resolves to 127.0.0.1 but sends its
/// own name in `Host`, which is never `localhost` or the bound IP.
pub fn host_allowed(host_header: &str, local: SocketAddr) -> bool {
    let host_header = host_header.trim();
    if host_header.is_empty() {
        return false;
    }
    let (host, port) = split_host_port(host_header);
    if let Some(port) = port
        && port != local.port()
    {
        return false;
    }
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let Ok(ip) = host.trim_matches('[').trim_matches(']').parse::<IpAddr>() else {
        return false;
    };
    if ip.is_loopback() {
        return true;
    }
    if local.ip().is_unspecified() {
        // Wildcard bind: private/LAN addresses are legitimate, public ones are
        // not (the server is meant for the local network only).
        return is_private(ip);
    }
    ip == local.ip()
}

fn split_host_port(host: &str) -> (&str, Option<u16>) {
    if let Some(rest) = host.strip_prefix('[') {
        if let Some((inner, tail)) = rest.split_once(']') {
            let port = tail
                .strip_prefix(':')
                .and_then(|port| port.parse::<u16>().ok());
            return (inner, port);
        }
        return (host, None);
    }
    match host.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => {
            (name, port.parse::<u16>().ok())
        }
        _ => (host, None),
    }
}

pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

impl Response {
    pub fn new(status: u16, content_type: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type,
            body: body.into(),
            headers: Vec::new(),
        }
    }

    pub fn html(status: u16, body: impl Into<Vec<u8>>) -> Self {
        let mut response = Self::new(status, "text/html; charset=utf-8", body.into());
        response.headers.push((
            "Content-Security-Policy".to_string(),
            "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; \
             img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'self'; \
             frame-ancestors 'none'"
                .to_string(),
        ));
        response
    }

    pub fn json(status: u16, value: &impl serde::Serialize) -> Self {
        let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{\"ok\":false}".to_vec());
        Self::new(status, "application/json; charset=utf-8", body)
    }

    pub fn text(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self::new(status, "text/plain; charset=utf-8", body)
    }

    pub fn redirect(location: &str) -> Self {
        let mut response = Self::text(303, Vec::new());
        response
            .headers
            .push(("Location".to_string(), location.to_string()));
        response
    }
}

pub fn write_response(stream: &mut TcpStream, response: &Response) -> io::Result<()> {
    let mut out = Vec::with_capacity(response.body.len() + 512);
    let reason = match response.status {
        200 => "OK",
        204 => "No Content",
        303 => "See Other",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        421 => "Misdirected Request",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "OK",
    };
    write!(
        out,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         X-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\n\
         Connection: close\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len()
    )?;
    for (name, value) in &response.headers {
        write!(out, "{name}: {value}\r\n")?;
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&response.body);
    stream.write_all(&out)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(host: &str) -> SocketAddr {
        SocketAddr::new(host.parse().unwrap(), 42800)
    }

    #[test]
    fn host_header_must_match_the_listener() {
        assert!(host_allowed("127.0.0.1:42800", local("127.0.0.1")));
        assert!(host_allowed("localhost:42800", local("127.0.0.1")));
        assert!(!host_allowed("localhost:5000", local("127.0.0.1")));
        // DNS rebinding: a public name must never be accepted.
        assert!(!host_allowed("evil.example:42800", local("127.0.0.1")));
        assert!(!host_allowed("192.168.1.10:42800", local("127.0.0.1")));
    }

    #[test]
    fn wildcard_bind_accepts_private_hosts_only() {
        let wildcard = local("0.0.0.0");
        assert!(host_allowed("192.168.1.10:42800", wildcard));
        assert!(host_allowed("10.0.0.5:42800", wildcard));
        assert!(host_allowed("127.0.0.1:42800", wildcard));
        assert!(!host_allowed("8.8.8.8:42800", wildcard));
        assert!(!host_allowed("evil.example:42800", wildcard));
    }

    #[test]
    fn percent_encoding_roundtrips() {
        assert_eq!(percent_decode("a%20b", false).unwrap(), "a b");
        assert_eq!(percent_decode("a+b", true).unwrap(), "a b");
        assert_eq!(percent_decode("a+b", false).unwrap(), "a+b");
        assert_eq!(percent_decode("%E4%B8%AD", false).unwrap(), "中");
        assert!(percent_decode("%zz", false).is_none());
        assert_eq!(percent_encode("中 a/b"), "%E4%B8%AD%20a%2Fb");
    }

    #[test]
    fn cookies_are_found_by_name() {
        let header = "a=1; bamboo_web_key=secret%20key; b=2";
        assert_eq!(cookie_value(header, "bamboo_web_key"), Some("secret%20key"));
        assert_eq!(cookie_value(header, "missing"), None);
    }

    #[test]
    fn secrets_compare_exactly() {
        assert!(secret_eq("abc", "abc"));
        assert!(!secret_eq("abc", "abd"));
        assert!(!secret_eq("abc", "abcd"));
    }
}

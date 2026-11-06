//! `.mp3list` / `.m3u` parsing and export.
//!
//! Every import re-expands `#SEARCH#` directories, so files added or removed
//! since the last open are reflected. Queue keys (not indices) are used to
//! restore the previous track and position.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::model::{Track, TrackSource, normalize_path};

#[derive(Debug, Default)]
pub struct ParsedPlaylist {
    pub dirs: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
    pub urls: Vec<String>,
    pub online: Vec<Track>,
    pub missing: usize,
    pub encoding: String,
}

pub fn parse(path: &Path) -> Result<ParsedPlaylist, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("无法读取歌单: {e}"))?;
    let (text, encoding) = decode_text(&bytes);
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let mut out = ParsedPlaylist {
        encoding,
        ..Default::default()
    };
    let mut seen = HashSet::new();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("#SEARCH#") {
            let raw_path = &line[8..];
            let value = expand_env(strip_quotes(raw_path.trim()));
            if value.is_empty() {
                continue;
            }
            let dir = resolve_path(base, &value);
            if !dir.is_dir() {
                out.missing += 1;
                continue;
            }
            if seen.insert(normalize_path(&dir)) {
                out.dirs.push(dir);
            }
            continue;
        }
        if upper.starts_with("#BAMBOO#") {
            if let Some(track) = parse_bamboo(&line[8..]) {
                if seen.insert(track.dedupe_key()) {
                    out.online.push(track);
                }
            }
            continue;
        }
        if line.starts_with('#') {
            continue; // comment / M3U directive
        }
        let value = expand_env(strip_quotes(line));
        if value.is_empty() {
            continue;
        }
        if value.starts_with("http://") || value.starts_with("https://") {
            if seen.insert(format!("url:{}", value.trim())) {
                out.urls.push(value);
            }
            continue;
        }
        if value.starts_with("file://") {
            if let Ok(url) = url::Url::parse(&value) {
                if let Ok(path) = url.to_file_path() {
                    if seen.insert(normalize_path(&path)) {
                        out.files.push(path);
                    }
                }
            }
            continue;
        }
        let file = resolve_path(base, &value);
        if !file.is_file() {
            out.missing += 1;
            continue;
        }
        if seen.insert(normalize_path(&file)) {
            out.files.push(file);
        }
    }

    Ok(out)
}

/// Export the current queue in `.mp3list` form (UTF-8, optional BOM).
pub fn export(path: &Path, tracks: &[Track], bom: bool) -> Result<usize, String> {
    let mut out = String::new();
    let mut count = 0usize;
    for track in tracks {
        let line = match &track.source {
            TrackSource::Local { path } => path.display().to_string(),
            TrackSource::Url { url } | TrackSource::Radio { url } => url.clone(),
            TrackSource::Bamboo {
                source_id, song_id, ..
            } => format!(
                "#BAMBOO#{}|{}|{}|{}",
                clean_field(source_id),
                clean_field(song_id),
                clean_field(&track.title),
                clean_field(&track.artist)
            ),
        };
        out.push_str(&line);
        out.push('\n');
        count += 1;
    }

    let mut bytes = Vec::with_capacity(out.len() + 3);
    if bom {
        bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    bytes.extend_from_slice(out.as_bytes());

    let tmp = path.with_extension("mp3list.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("无法写入歌单: {e}"))?;
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path).map_err(|e| format!("无法写入歌单: {e}"))?;
    }
    Ok(count)
}

fn parse_bamboo(rest: &str) -> Option<Track> {
    let mut parts = rest.split('|');
    let source_id = parts.next().unwrap_or("online").trim().to_string();
    let song_id = parts.next().unwrap_or("").trim().to_string();
    if song_id.is_empty() {
        return None;
    }
    let title = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&song_id)
        .to_string();
    let artist = parts.next().unwrap_or("").trim().to_string();
    Some(Track {
        source: TrackSource::Bamboo {
            source_id,
            song_id,
            base_url: String::new(),
        },
        title,
        artist,
        source_name: "在线".to_string(),
        ..Default::default()
    })
}

fn clean_field(value: &str) -> String {
    value
        .replace('|', "/")
        .replace(['\r', '\n'], " ")
        .trim()
        .to_string()
}

fn resolve_path(base: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn strip_quotes(value: &str) -> &str {
    let value = value.trim();
    if value.len() >= 2 {
        let first = value.as_bytes()[0];
        let last = value.as_bytes()[value.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn expand_env(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let name = &after[..end];
            if let Some(var) = std::env::var_os(name) {
                out.push_str(&var.to_string_lossy());
            } else {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
            rest = &after[end + 1..];
        } else {
            out.push('%');
            out.push_str(after);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

fn decode_text(bytes: &[u8]) -> (String, String) {
    use encoding_rs::{GB18030, UTF_16BE, UTF_16LE, UTF_8};

    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (UTF_8.decode(&bytes[3..]).0.into_owned(), "UTF-8 BOM".to_string());
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return (
            UTF_16LE.decode(&bytes[2..]).0.into_owned(),
            "UTF-16 LE".to_string(),
        );
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return (
            UTF_16BE.decode(&bytes[2..]).0.into_owned(),
            "UTF-16 BE".to_string(),
        );
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return (text.to_string(), "UTF-8".to_string());
    }

    let mut detector = chardetng::EncodingDetector::new(
        chardetng::Iso2022JpDetection::Deny,
    );
    detector.feed(bytes, true);
    let encoding = detector.guess(None, chardetng::Utf8Detection::Allow);
    let (text, _, _) = encoding.decode(bytes);
    let name = encoding.name().to_string();
    if text.contains('\u{FFFD}') {
        let (fallback, _, _) = GB18030.decode(bytes);
        return (fallback.into_owned(), "GB18030".to_string());
    }
    (text.into_owned(), name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_search_files_urls_and_bamboo() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let music = root.join("music");
        let sub = music.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let a = music.join("a.mp3");
        let b = sub.join("b.mp3");
        let other = root.join("other.flac");
        for path in [&a, &b, &other] {
            std::fs::write(path, b"x").unwrap();
        }
        let list = root.join("list.mp3list");
        std::fs::write(
            &list,
            format!(
                "# comment\n#SEARCH#music\nother.flac\nmissing.mp3\nhttps://example.com/s.mp3\n#BAMBOO#online|song1|Title|Artist\n"
            ),
        )
        .unwrap();

        let parsed = parse(&list).unwrap();
        assert_eq!(parsed.dirs, vec![music]);
        assert_eq!(parsed.files, vec![other]);
        assert_eq!(parsed.urls, vec!["https://example.com/s.mp3"]);
        assert_eq!(parsed.online.len(), 1);
        assert_eq!(parsed.online[0].dedupe_key(), "bamboo:online:song1");
        assert_eq!(parsed.missing, 1);
    }

    #[test]
    fn detects_gbk_playlist() {
        use encoding_rs::GBK;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let chinese = root.join("音乐.mp3");
        std::fs::write(&chinese, b"x").unwrap();
        let list = root.join("gbk.mp3list");
        let text = format!("{}\n", chinese.display());
        let (bytes, _, _) = GBK.encode(&text);
        std::fs::write(&list, bytes).unwrap();

        let parsed = parse(&list).unwrap();
        assert_eq!(parsed.files.len(), 1);
        assert!(
            parsed.encoding.to_lowercase().contains("gbk")
                || parsed.encoding.to_lowercase().contains("gb18030")
        );
    }

    #[test]
    fn exports_and_reimports() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let local = root.join("track.mp3");
        std::fs::write(&local, b"x").unwrap();
        let tracks = vec![
            Track::local(local.clone()),
            Track {
                source: TrackSource::Url {
                    url: "https://example.com/a.mp3".to_string(),
                },
                title: "a".to_string(),
                ..Default::default()
            },
            Track {
                source: TrackSource::Bamboo {
                    source_id: "online".to_string(),
                    song_id: "42".to_string(),
                    base_url: String::new(),
                },
                title: "Bamboo Track".to_string(),
                artist: "Singer".to_string(),
                ..Default::default()
            },
        ];
        let list = root.join("export.mp3list");
        assert_eq!(export(&list, &tracks, true).unwrap(), 3);

        let bytes = std::fs::read(&list).unwrap();
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);

        let parsed = parse(&list).unwrap();
        assert_eq!(parsed.files, vec![local]);
        assert_eq!(parsed.urls.len(), 1);
        assert_eq!(parsed.online.len(), 1);
        assert_eq!(parsed.online[0].title, "Bamboo Track");
    }
}

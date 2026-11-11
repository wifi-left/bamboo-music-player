//! `.mp3list` / `.m3u` parsing and export.
//!
//! Every import re-expands `#SEARCH#` directories, so files added or removed
//! since the last open are reflected. Queue keys (not indices) are used to
//! restore the previous track and position.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::model::{Track, TrackSource, normalize_path};

/// A `#SEARCH#`/`#SEARCHA#` directory plus its optional `#type#` filter.
#[derive(Debug, Clone)]
pub struct PlaylistDir {
    pub path: PathBuf,
    /// Empty means the application's default audio extension list.
    pub extensions: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ParsedPlaylist {
    pub dirs: Vec<PlaylistDir>,
    pub files: Vec<PathBuf>,
    pub urls: Vec<String>,
    pub online: Vec<Track>,
    pub missing: usize,
    pub encoding: String,
}

pub fn parse(path: &Path) -> Result<ParsedPlaylist, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("无法读取歌单: {e}"))?;
    let base = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let (text, encoding) = decode_best(&bytes, &base);
    let mut out = ParsedPlaylist {
        encoding,
        ..Default::default()
    };
    let mut seen_files = HashSet::new();
    let mut seen_urls = HashSet::new();
    let mut seen_online = HashSet::new();
    let mut dir_index: HashMap<String, usize> = HashMap::new();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((raw_path, type_filter)) = parse_search_line(line) {
            let value = expand_env(strip_quotes(raw_path.trim()));
            if value.is_empty() {
                continue;
            }
            let dir = resolve_path(&base, &value);
            if !dir.is_dir() {
                out.missing += 1;
                continue;
            }
            let extensions = type_filter.map(parse_type_filter).unwrap_or_default();
            let key = normalize_path(&dir);
            if let Some(&index) = dir_index.get(&key) {
                let existing = &mut out.dirs[index];
                if existing.extensions.is_empty() {
                    // Already unfiltered; keep it that way.
                } else if extensions.is_empty() {
                    existing.extensions.clear();
                } else {
                    for ext in extensions {
                        if !existing.extensions.contains(&ext) {
                            existing.extensions.push(ext);
                        }
                    }
                }
            } else {
                dir_index.insert(key, out.dirs.len());
                out.dirs.push(PlaylistDir {
                    path: dir,
                    extensions,
                });
            }
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("#BAMBOO#") {
            if let Some(track) = parse_bamboo(&line[8..]) {
                if seen_online.insert(track.dedupe_key()) {
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
            if seen_urls.insert(value.trim().to_string()) {
                out.urls.push(value);
            }
            continue;
        }
        if value.starts_with("file://") {
            if let Ok(url) = url::Url::parse(&value) {
                if let Ok(path) = url.to_file_path() {
                    if seen_files.insert(normalize_path(&path)) {
                        out.files.push(path);
                    }
                }
            }
            continue;
        }
        let file = resolve_path(&base, &value);
        if !file.is_file() {
            out.missing += 1;
            continue;
        }
        if seen_files.insert(normalize_path(&file)) {
            out.files.push(file);
        }
    }

    Ok(out)
}

/// Parse the real-world `#SEARCHA#<dir>#type#*.mp3` form as well as the
/// documented `#SEARCH#<dir>` form.
fn parse_search_line(line: &str) -> Option<(&str, Option<&str>)> {
    let upper = line.to_ascii_uppercase();
    if !upper.starts_with("#SEARCH") {
        return None;
    }
    if line.len() < 8 {
        return None;
    }
    // Skip "#SEARCH" and any suffix letters ("A", "B", ...), then the '#'.
    let rest = &line[7..];
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let rest = rest.strip_prefix('#')?;
    let lower = rest.to_ascii_lowercase();
    if let Some(pos) = lower.find("#type#") {
        let path = rest[..pos].trim();
        let filter = rest[pos + 6..].trim();
        Some((path, Some(filter)))
    } else {
        Some((rest.trim(), None))
    }
}

/// `#type#*.mp3;*.flac` -> `["mp3", "flac"]`. `*` / `*.*` means all supported
/// audio formats, represented by an empty list.
fn parse_type_filter(filter: &str) -> Vec<String> {
    let mut extensions = Vec::new();
    for part in filter.split([';', ',', '|', ' ']) {
        let part = part.trim().trim_matches('"').to_ascii_lowercase();
        if part.is_empty() {
            continue;
        }
        if part == "*" || part == "*.*" {
            return Vec::new();
        }
        let ext = part
            .strip_prefix("*.")
            .or_else(|| part.strip_prefix('.'))
            .unwrap_or(&part);
        if !ext.is_empty() && !ext.contains(['*', '?']) {
            if !extensions.iter().any(|e| e == ext) {
                extensions.push(ext.to_string());
            }
        }
    }
    extensions
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

/// Decode a playlist that may be UTF-8 (with or without BOM) or a legacy
/// Chinese/Japanese code page. Several candidates are scored so a GBK file
/// containing Japanese folder names is not misdetected as Shift-JIS.
fn decode_best(bytes: &[u8], base: &Path) -> (String, String) {
    use encoding_rs::{BIG5, GB18030, SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE};

    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (
            UTF_8.decode(&bytes[3..]).0.into_owned(),
            "UTF-8 BOM".to_string(),
        );
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

    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    let detected = detector.guess(None, chardetng::Utf8Detection::Allow);

    let mut candidates: Vec<&'static encoding_rs::Encoding> = vec![detected];
    for encoding in [GB18030, BIG5, SHIFT_JIS] {
        if !candidates.iter().any(|e| e.name() == encoding.name()) {
            candidates.push(encoding);
        }
    }

    let mut best: Option<((i32, i32, i32), String, String)> = None;
    for (order, encoding) in candidates.into_iter().enumerate() {
        let (text, _, had_errors) = encoding.decode(bytes);
        let text = text.into_owned();
        let replacements = text.matches('\u{FFFD}').count() as i32 + i32::from(had_errors);
        let score = candidate_score(&text, base);
        // Higher score, fewer replacement characters, earlier preference.
        let rank = (score, -replacements, -(order as i32));
        let name = encoding.name().to_string();
        if best
            .as_ref()
            .map(|(current, _, _)| rank > *current)
            .unwrap_or(true)
        {
            best = Some((rank, text, name));
        }
    }

    match best {
        Some((_, text, name)) => (text, name),
        None => (
            String::from_utf8_lossy(bytes).into_owned(),
            "UTF-8 (lossy)".to_string(),
        ),
    }
}

/// Score a decoded candidate by how many referenced paths exist, then by how
/// plausible the CJK characters look. GBK-misread-as-Shift-JIS text is full of
/// half-width katakana and scores poorly.
fn candidate_score(text: &str, base: &Path) -> i32 {
    let mut score = 0i32;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((raw_path, _)) = parse_search_line(line) {
            let value = expand_env(strip_quotes(raw_path.trim()));
            if !value.is_empty() && resolve_path(base, &value).is_dir() {
                score += 120;
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let value = expand_env(strip_quotes(line));
        if value.is_empty()
            || value.starts_with("http://")
            || value.starts_with("https://")
            || value.starts_with("file://")
        {
            continue;
        }
        if resolve_path(base, &value).is_file() {
            score += 120;
        }
    }

    for ch in text.chars() {
        let code = ch as u32;
        score += match code {
            0x4E00..=0x9FFF => 2,  // CJK ideographs
            0x3040..=0x30FF => 2,  // kana
            0xFF61..=0xFF9F => -4, // half-width katakana (typical mojibake)
            0xFFFD => -20,
            _ => 0,
        };
    }
    score
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
        assert_eq!(parsed.dirs.len(), 1);
        assert_eq!(parsed.dirs[0].path, music);
        assert!(parsed.dirs[0].extensions.is_empty());
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
    fn parses_searcha_with_type_filter() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let touhou = root.join("Touhou");
        let other = root.join("other");
        std::fs::create_dir_all(&touhou).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let list = root.join("searcha.mp3list");
        std::fs::write(
            &list,
            format!(
                "#SEARCHA#{}#type#*.mp3;*.flac\n#SEARCH#{}#type#*\n",
                touhou.display(),
                other.display()
            ),
        )
        .unwrap();

        let parsed = parse(&list).unwrap();
        assert_eq!(parsed.dirs.len(), 2);
        assert_eq!(parsed.dirs[0].path, touhou);
        assert_eq!(parsed.dirs[0].extensions, vec!["mp3", "flac"]);
        assert!(parsed.dirs[1].extensions.is_empty());
    }

    #[test]
    fn detects_gbk_with_japanese_and_chinese_names() {
        use encoding_rs::GB18030;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let japanese = root.join("ししまいブラザーズ");
        let chinese = root.join("狐尾草");
        std::fs::create_dir_all(&japanese).unwrap();
        std::fs::create_dir_all(&chinese).unwrap();
        let list = root.join("legacy.mp3list");
        let text = format!(
            "#SEARCHA#{}#type#*.mp3\r\n#SEARCHA#{}#type#*.mp3\r\n",
            japanese.display(),
            chinese.display()
        );
        let (bytes, _, _) = GB18030.encode(&text);
        std::fs::write(&list, bytes).unwrap();

        let parsed = parse(&list).unwrap();
        assert_eq!(parsed.dirs.len(), 2);
        assert_eq!(parsed.dirs[0].path, japanese);
        assert_eq!(parsed.dirs[1].path, chinese);
        assert!(!parsed.encoding.to_lowercase().contains("shift"));
    }

    /// Validates the real `#SEARCHA#...#type#*.mp3` files the user keeps in
    /// `E:\playlist`; silently skipped on machines without that folder.
    #[test]
    fn parses_real_example_playlists_if_present() {
        let dir = Path::new("E:\\playlist");
        if !dir.is_dir() {
            return;
        }
        for name in ["ALL.mp3list", "Touhou.mp3list", "Touhou_vocal.mp3list"] {
            let path = dir.join(name);
            if !path.is_file() {
                continue;
            }
            let parsed = parse(&path).unwrap();
            assert!(
                !parsed.dirs.is_empty(),
                "{name} should contain #SEARCHA# directories"
            );
            assert!(
                parsed.dirs.iter().all(|d| d.path.is_dir()),
                "{name} paths should resolve"
            );
            assert!(
                parsed.dirs.iter().any(|d| d.extensions == vec!["mp3"]),
                "{name} should carry the #type#*.mp3 filter"
            );
            assert!(
                !parsed.encoding.to_lowercase().contains("shift"),
                "{name} is GBK, not Shift-JIS (got {})",
                parsed.encoding
            );
        }
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

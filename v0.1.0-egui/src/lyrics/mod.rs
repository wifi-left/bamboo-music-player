//! Lyrics model, encoding detection and LRC/SRT parsing.

use std::path::{Path, PathBuf};

use encoding_rs::{BIG5, GB18030, GBK, SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE};

use crate::config::LyricsEncoding;

pub mod editor;
pub mod lrc;
pub mod service;
pub mod srt;

pub use editor::LyricsEditor;
pub use service::{LyricsResult, LyricsService};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WordTiming {
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct LyricLine {
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub text: String,
    pub translation: Option<String>,
    pub words: Vec<WordTiming>,
}

#[derive(Clone, Debug, Default)]
pub struct Lyrics {
    pub lines: Vec<LyricLine>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub offset_ms: i64,
    pub source: LyricsSource,
    pub path: Option<PathBuf>,
    /// Translation sidecar (`A.zh.lrc`, `A.srt`, …) that supplied the
    /// `translation` fields, if any.
    pub translation_path: Option<PathBuf>,
    pub encoding: Option<String>,
    /// False for plain-text (untimed) lyrics.
    pub timed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LyricsSource {
    #[default]
    None,
    Sidecar,
    Embedded,
    Bamboo,
    Manual,
    Edited,
}

impl LyricsSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::None => "",
            Self::Sidecar => crate::i18n::tr("同名歌词文件"),
            Self::Embedded => crate::i18n::tr("内嵌歌词"),
            Self::Bamboo => crate::i18n::tr("在线歌词"),
            Self::Manual => crate::i18n::tr("手动选择"),
            Self::Edited => crate::i18n::tr("已编辑"),
        }
    }
}

impl Lyrics {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Binary search for the line active at `position_ms`.
    pub fn index_at(&self, position_ms: u64) -> Option<usize> {
        if self.lines.is_empty() || !self.timed {
            return None;
        }
        let adjusted = (position_ms as i64 + self.offset_ms).max(0) as u64;
        let mut lo = 0usize;
        let mut hi = self.lines.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.lines[mid].start_ms <= adjusted {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 { None } else { Some(lo - 1) }
    }

}

/// Karaoke progress for one line.
pub struct KaraokeProgress {
    /// Text that has already been sung.
    pub sung: String,
    /// The character being sung right now, if any.
    pub next: Option<char>,
    /// Progress through `next` in `0.0..1.0`; used for pixel-level fill.
    pub next_fraction: f32,
}

/// Compute karaoke progress using word timings when available, otherwise the
/// line's own duration. The split is by character, with `next_fraction` adding
/// sub-character precision for clipping-based rendering.
pub fn karaoke_progress(line: &LyricLine, position_ms: u64, line_end_ms: u64) -> KaraokeProgress {
    let chars: Vec<char> = line.text.chars().collect();
    let total = chars.len();
    let (done, fraction) = if !line.words.is_empty() {
        let mut done = 0usize;
        let mut fraction = 0.0f32;
        for (index, word) in line.words.iter().enumerate() {
            let word_chars = word.text.chars().count();
            let word_end = word
                .end_ms
                .unwrap_or_else(|| {
                    line.words
                        .get(index + 1)
                        .map(|next| next.start_ms)
                        .unwrap_or(line_end_ms)
                })
                .max(word.start_ms);
            if position_ms >= word_end {
                done += word_chars;
                continue;
            }
            if position_ms > word.start_ms {
                let span = word_end.saturating_sub(word.start_ms).max(1);
                let exact = word_chars as f64 * (position_ms - word.start_ms) as f64 / span as f64;
                let whole = exact.floor() as usize;
                done += whole.min(word_chars);
                fraction = (exact - whole as f64) as f32;
            }
            break;
        }
        (done, fraction)
    } else {
        let span = line_end_ms.saturating_sub(line.start_ms).max(1);
        let elapsed = position_ms.saturating_sub(line.start_ms).min(span);
        let exact = total as f64 * elapsed as f64 / span as f64;
        let whole = exact.floor() as usize;
        (whole, (exact - whole as f64) as f32)
    };
    let done = done.min(total);
    KaraokeProgress {
        sung: chars.iter().take(done).collect(),
        next: chars.get(done).copied(),
        next_fraction: fraction.clamp(0.0, 1.0),
    }
}

/// Split a line into `(text, sung)` runs for karaoke coloring. The
/// concatenation of all runs is always exactly `line.text`.
pub fn karaoke_runs(line: &LyricLine, position_ms: u64, line_end_ms: u64) -> Vec<(String, bool)> {
    let progress = karaoke_progress(line, position_ms, line_end_ms);
    let done_chars = progress.sung.chars().count();
    let mut runs = Vec::with_capacity(2);
    if !progress.sung.is_empty() {
        runs.push((progress.sung, true));
    }
    let rest: String = line.text.chars().skip(done_chars).collect();
    if !rest.is_empty() {
        runs.push((rest, false));
    }
    runs
}

pub fn parse_text(
    text: &str,
    source: LyricsSource,
    path: Option<PathBuf>,
    encoding: Option<String>,
) -> Lyrics {
    let trimmed = text.trim_start_matches('\u{feff}');
    if trimmed.contains("-->") {
        let lyrics = srt::parse(trimmed, source.clone(), path.clone(), encoding.clone());
        if !lyrics.lines.is_empty() {
            return lyrics;
        }
    }
    if looks_like_lrc(trimmed) {
        let lyrics = lrc::parse(trimmed, source.clone(), path.clone(), encoding.clone());
        if !lyrics.lines.is_empty() {
            return lyrics;
        }
    }
    parse_plain(trimmed, source, path, encoding)
}

fn looks_like_lrc(text: &str) -> bool {
    text.lines().take(200).any(|line| {
        let line = line.trim_start();
        if !line.starts_with('[') {
            return false;
        }
        let Some(end) = line.find(']') else {
            return false;
        };
        lrc::parse_time_tag(&line[1..end]).is_some()
    })
}

fn parse_plain(
    text: &str,
    source: LyricsSource,
    path: Option<PathBuf>,
    encoding: Option<String>,
) -> Lyrics {
    let mut lyrics = Lyrics {
        source,
        path,
        encoding,
        timed: false,
        ..Default::default()
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        lyrics.lines.push(LyricLine {
            start_ms: 0,
            end_ms: None,
            text: line.to_string(),
            translation: None,
            words: Vec::new(),
        });
    }
    lyrics
}

pub fn decode_bytes(bytes: &[u8], setting: LyricsEncoding) -> (String, String) {
    let (bytes, bom_encoding) = strip_bom(bytes);
    if let Some(encoding) = bom_encoding {
        let (text, _, _) = encoding.decode(bytes);
        return (text.into_owned(), encoding.name().to_string());
    }

    let forced = match setting {
        LyricsEncoding::Auto => None,
        LyricsEncoding::Utf8 => Some(UTF_8),
        LyricsEncoding::Gbk => Some(GBK),
        LyricsEncoding::Gb18030 => Some(GB18030),
        LyricsEncoding::Utf16Le => Some(UTF_16LE),
        LyricsEncoding::Utf16Be => Some(UTF_16BE),
        LyricsEncoding::Big5 => Some(BIG5),
    };
    if let Some(encoding) = forced {
        let (text, _, _) = encoding.decode(bytes);
        return (text.into_owned(), encoding.name().to_string());
    }

    if let Ok(text) = std::str::from_utf8(bytes) {
        return (text.to_string(), "UTF-8".to_string());
    }

    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    let detected = detector.guess(None, chardetng::Utf8Detection::Allow);
    let mut candidates: Vec<&'static encoding_rs::Encoding> = vec![detected];
    for encoding in [GB18030, BIG5, SHIFT_JIS] {
        if !candidates.iter().any(|item| item.name() == encoding.name()) {
            candidates.push(encoding);
        }
    }

    let mut best: Option<((i32, i32, i32), String, String)> = None;
    for (order, encoding) in candidates.into_iter().enumerate() {
        let (text, _, had_errors) = encoding.decode(bytes);
        let text = text.into_owned();
        let replacements = text.matches('\u{FFFD}').count() as i32 + i32::from(had_errors);
        let score = text_score(&text);
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

fn strip_bom(bytes: &[u8]) -> (&[u8], Option<&'static encoding_rs::Encoding>) {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (&bytes[3..], Some(UTF_8))
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        (&bytes[2..], Some(UTF_16LE))
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        (&bytes[2..], Some(UTF_16BE))
    } else {
        (bytes, None)
    }
}

fn text_score(text: &str) -> i32 {
    let mut score = 0i32;
    for ch in text.chars() {
        score += match ch as u32 {
            0x4E00..=0x9FFF => 2,
            0x3040..=0x30FF => 2,
            0xFF61..=0xFF9F => -4,
            0xFFFD => -20,
            _ => 0,
        };
    }
    score
}

/// Same-name `.lrc` (preferred) or `.srt` beside the audio file.
pub fn find_sidecar(audio: &Path) -> Option<PathBuf> {
    for extension in ["lrc", "srt"] {
        let candidate = audio.with_extension(extension);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Case-insensitive fallback for unusual file names.
    let directory = audio.parent()?;
    let stem = audio.file_stem()?.to_string_lossy().to_ascii_lowercase();
    let entries = std::fs::read_dir(directory).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension() else {
            continue;
        };
        let ext = ext.to_string_lossy().to_ascii_lowercase();
        if !matches!(ext.as_str(), "lrc" | "srt") {
            continue;
        }
        let file_stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if file_stem == stem {
            return Some(path);
        }
    }
    None
}

/// Canonical translation region name. `None` means "auto".
///
/// The sidecar layout is `A.zh.lrc`, `A.en.lrc`, `A.jp.lrc` beside `A.mp3`
/// (the same stem with the region inserted before the lyrics extension).
pub fn normalize_region(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase().replace('_', "-");
    match value.as_str() {
        "" | "auto" => None,
        "zh" | "zh-cn" | "zh-hans" | "chinese" | "中文" => Some("zh".to_string()),
        "en" | "en-us" | "en-gb" | "english" => Some("en".to_string()),
        "jp" | "ja" | "ja-jp" | "japanese" | "日本語" => Some("jp".to_string()),
        other
            if !other.is_empty()
                && other.len() <= 12
                && other
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-') =>
        {
            Some(other.to_string())
        }
        _ => None,
    }
}

/// Region used when the setting is "auto": the UI language, then any common
/// translation sidecar.
pub fn resolve_region(preferred: &str) -> String {
    normalize_region(preferred).unwrap_or_else(|| {
        match crate::i18n::current_language() {
            crate::config::Language::EnUs => "en",
            crate::config::Language::ZhCn => "zh",
        }
        .to_string()
    })
}

/// Candidate regions in lookup order: the configured language first, then the
/// UI language, then the common regions. This keeps "default language" useful
/// while still showing a translation when that language is not present.
fn translation_regions(preferred: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(region) = normalize_region(preferred) {
        out.push(region);
    }
    let resolved = resolve_region(preferred);
    if !out.contains(&resolved) {
        out.push(resolved);
    }
    for region in ["zh", "en", "jp"] {
        if !out.iter().any(|item| item == region) {
            out.push(region.to_string());
        }
    }
    out
}

/// Path used to write a translation in `region` for `base` (`A.lrc` ->
/// `A.zh.lrc`). Translation sidecars are always LRC, even for an SRT base.
pub fn translation_sidecar_for(base: &Path, region: &str) -> PathBuf {
    let region = resolve_region(region);
    let stem = base
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "lyrics".to_string());
    base.with_file_name(format!("{stem}.{region}.lrc"))
}

/// Find the translation sidecar for `base` (an audio file or its base lyrics
/// file). Returns the selected region and path.
pub fn find_translation_sidecar(base: &Path, preferred: &str) -> Option<(String, PathBuf)> {
    let regions = translation_regions(preferred);
    let stem = base.file_stem()?.to_string_lossy().into_owned();
    for region in &regions {
        for extension in ["lrc", "srt"] {
            let candidate = base.with_file_name(format!("{stem}.{region}.{extension}"));
            if candidate.is_file() {
                return Some((region.clone(), candidate));
            }
        }
    }

    // Case-insensitive fallback: map every `stem.region.ext` entry back to a
    // region, then pick it in the configured order.
    let directory = base.parent()?;
    let prefix = format!("{}.", stem.to_ascii_lowercase());
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(directory).ok()?.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension() else {
            continue;
        };
        let ext = ext.to_string_lossy().to_ascii_lowercase();
        if !matches!(ext.as_str(), "lrc" | "srt") {
            continue;
        }
        let file_stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let Some(raw_region) = file_stem.strip_prefix(&prefix) else {
            continue;
        };
        if let Some(region) = normalize_region(raw_region) {
            found.push((region, path));
        }
    }
    for region in &regions {
        if let Some((_, path)) = found.iter().find(|(found, _)| found == region) {
            return Some((region.clone(), path.clone()));
        }
    }
    None
}

/// Attach a translation document to `base` by timestamp (or by line index for
/// untimed lyrics). Multiple translation lines sharing a timestamp are joined
/// with newlines so a bilingual SRT/LRC stays readable as one pair.
pub fn merge_translation(base: &mut Lyrics, translation: &Lyrics) {
    if translation.lines.is_empty() {
        return;
    }
    if base.timed {
        let mut by_time: std::collections::HashMap<u64, Vec<String>> =
            std::collections::HashMap::new();
        for line in &translation.lines {
            let mut parts = Vec::new();
            let original = line.text.trim();
            if !original.is_empty() {
                parts.push(original.to_string());
            }
            if let Some(extra) = line.translation.as_deref().map(str::trim)
                && !extra.is_empty()
            {
                parts.push(extra.to_string());
            }
            if !parts.is_empty() {
                by_time
                    .entry(line.start_ms)
                    .or_default()
                    .extend(parts.into_iter());
            }
        }
        for line in &mut base.lines {
            if let Some(parts) = by_time.get(&line.start_ms) {
                line.translation = Some(parts.join("\n"));
            }
        }
    } else if base.lines.len() == translation.lines.len() {
        for (line, translated) in base.lines.iter_mut().zip(translation.lines.iter()) {
            let mut text = translated.text.trim().to_string();
            if let Some(extra) = translated.translation.as_deref().map(str::trim)
                && !extra.is_empty()
            {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(extra);
            }
            if !text.is_empty() {
                line.translation = Some(text);
            }
        }
    } else {
        for (line, translated) in base.lines.iter_mut().zip(translation.lines.iter()) {
            let text = translated.text.trim();
            if !text.is_empty() {
                line.translation = Some(text.to_string());
            }
        }
    }
    base.translation_path = translation.path.clone();
}

pub fn load_file(
    path: &Path,
    encoding: LyricsEncoding,
    source: LyricsSource,
) -> Result<Lyrics, String> {
    let bytes = std::fs::read(path).map_err(|err| crate::trf!("无法读取歌词: {}", err))?;
    let (text, label) = decode_bytes(&bytes, encoding);
    let mut lyrics = parse_text(&text, source, Some(path.to_path_buf()), Some(label));
    if lyrics.is_empty() {
        return Err(crate::i18n::tr("歌词文件为空或格式无法识别").to_string());
    }
    if lyrics.path.is_none() {
        lyrics.path = Some(path.to_path_buf());
    }
    Ok(lyrics)
}

/// Load a base lyrics file and attach `stem.<region>.lrc/srt` if present.
pub fn load_file_with_translation(
    path: &Path,
    encoding: LyricsEncoding,
    source: LyricsSource,
    preferred_region: &str,
) -> Result<Lyrics, String> {
    let mut lyrics = load_file(path, encoding, source)?;
    if let Some((_, translation_path)) = find_translation_sidecar(path, preferred_region)
        && let Ok(translation) = load_file(&translation_path, encoding, LyricsSource::Sidecar)
    {
        merge_translation(&mut lyrics, &translation);
    }
    Ok(lyrics)
}

/// Sidecar first, then embedded tag lyrics. Translation sidecars
/// (`A.zh.lrc`, `A.en.lrc`, …) are merged into the base document.
pub fn load_for_audio(
    audio: &Path,
    encoding: LyricsEncoding,
    preferred_region: &str,
) -> Result<Lyrics, String> {
    if let Some(path) = find_sidecar(audio) {
        return load_file_with_translation(
            &path,
            encoding,
            LyricsSource::Sidecar,
            preferred_region,
        );
    }
    if let Some(text) = crate::util::metadata::read_embedded_lyrics(audio) {
        let mut lyrics = parse_text(&text, LyricsSource::Embedded, None, None);
        if !lyrics.is_empty() {
            lyrics.source = LyricsSource::Embedded;
            if let Some((_, translation_path)) = find_translation_sidecar(audio, preferred_region)
                && let Ok(translation) =
                    load_file(&translation_path, encoding, LyricsSource::Sidecar)
            {
                merge_translation(&mut lyrics, &translation);
            }
            return Ok(lyrics);
        }
    }
    Err(crate::i18n::tr("未找到同名 .lrc/.srt 或内嵌歌词").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf8_and_gbk() {
        let (text, label) = decode_bytes("你好".as_bytes(), LyricsEncoding::Auto);
        assert_eq!(text, "你好");
        assert_eq!(label, "UTF-8");

        let (bytes, _, _) = GBK.encode("[00:01.00]第一句\n");
        let (text, label) = decode_bytes(&bytes, LyricsEncoding::Auto);
        assert_eq!(text.trim(), "[00:01.00]第一句");
        assert!(label.to_lowercase().contains("gbk") || label.to_lowercase().contains("gb18030"));
    }

    #[test]
    fn forced_encoding_overrides_detection() {
        let (bytes, _, _) = GBK.encode("[00:01.00]第一句\n");
        let (text, _) = decode_bytes(&bytes, LyricsEncoding::Gbk);
        assert_eq!(text.trim(), "[00:01.00]第一句");
    }

    #[test]
    fn plain_text_is_not_timed() {
        let lyrics = parse_text("line one\nline two\n", LyricsSource::Embedded, None, None);
        assert!(!lyrics.timed);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.index_at(10_000), None);
    }

    #[test]
    fn karaoke_runs_split_the_active_word() {
        let line = LyricLine {
            start_ms: 0,
            end_ms: Some(1000),
            text: "Hello world".to_string(),
            words: vec![
                WordTiming {
                    start_ms: 0,
                    end_ms: Some(500),
                    text: "Hello ".to_string(),
                },
                WordTiming {
                    start_ms: 500,
                    end_ms: Some(1000),
                    text: "world".to_string(),
                },
            ],
            ..Default::default()
        };
        let runs = karaoke_runs(&line, 250, 1000);
        assert_eq!(
            runs,
            vec![("Hel".to_string(), true), ("lo world".to_string(), false),]
        );
        // The concatenation is byte-for-byte the original line.
        let joined: String = runs.into_iter().map(|(text, _)| text).collect();
        assert_eq!(joined, line.text);

        let progress = karaoke_progress(&line, 750, 1000);
        assert_eq!(progress.sung, "Hello wo");
        assert_eq!(progress.next, Some('r'));
        assert!((progress.next_fraction - 0.5).abs() < 1e-6);
    }

    #[test]
    fn karaoke_runs_fall_back_to_line_progress() {
        let line = LyricLine {
            start_ms: 1000,
            end_ms: Some(3000),
            text: "abcde".to_string(),
            ..Default::default()
        };
        let runs = karaoke_runs(&line, 1800, 3000);
        assert_eq!(
            runs,
            vec![("ab".to_string(), true), ("cde".to_string(), false)]
        );
        assert_eq!(
            karaoke_runs(&line, 0, 3000),
            vec![("abcde".to_string(), false)]
        );
        assert_eq!(
            karaoke_runs(&line, 9000, 3000),
            vec![("abcde".to_string(), true)]
        );
    }

    #[test]
    fn loads_translation_sidecar_for_audio() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Song.mp3");
        std::fs::write(&audio, b"x").unwrap();
        std::fs::write(
            dir.path().join("Song.lrc"),
            "[00:01.00]Hello\n[00:02.00]World\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Song.zh.lrc"),
            "[00:01.00]你好\n[00:02.00]世界\n",
        )
        .unwrap();
        let lyrics = load_for_audio(&audio, LyricsEncoding::Auto, "zh").unwrap();
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].translation.as_deref(), Some("你好"));
        assert_eq!(lyrics.lines[1].translation.as_deref(), Some("世界"));
        assert!(
            lyrics
                .translation_path
                .as_ref()
                .is_some_and(|path| path.ends_with("Song.zh.lrc"))
        );
    }

    #[test]
    fn translation_region_preference_wins_then_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Song.mp3");
        std::fs::write(&audio, b"x").unwrap();
        std::fs::write(dir.path().join("Song.lrc"), "[00:01.00]Hello\n").unwrap();
        std::fs::write(dir.path().join("Song.en.lrc"), "[00:01.00]Hi\n").unwrap();
        std::fs::write(dir.path().join("Song.jp.lrc"), "[00:01.00]こんにちは\n").unwrap();

        let english = load_for_audio(&audio, LyricsEncoding::Auto, "en").unwrap();
        assert_eq!(english.lines[0].translation.as_deref(), Some("Hi"));

        // The requested language is missing: fall back to another available
        // translation instead of dropping the bilingual display.
        let chinese = load_for_audio(&audio, LyricsEncoding::Auto, "zh").unwrap();
        assert!(chinese.lines[0].translation.is_some());
    }

    #[test]
    fn translation_sidecar_joins_duplicate_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Song.mp3");
        std::fs::write(&audio, b"x").unwrap();
        std::fs::write(dir.path().join("Song.lrc"), "[00:01.00]Hello\n").unwrap();
        std::fs::write(
            dir.path().join("Song.zh.lrc"),
            "[00:01.00]你好\n[00:01.00]第二行\n",
        )
        .unwrap();
        let lyrics = load_for_audio(&audio, LyricsEncoding::Auto, "zh").unwrap();
        assert_eq!(lyrics.lines[0].translation.as_deref(), Some("你好\n第二行"));
    }

    #[test]
    fn translation_sidecar_name_uses_region() {
        let base = Path::new("C:/music/Song.lrc");
        assert_eq!(
            translation_sidecar_for(base, "zh"),
            PathBuf::from("C:/music/Song.zh.lrc")
        );
        assert_eq!(
            translation_sidecar_for(base, "auto"),
            base.with_file_name(format!("Song.{}.lrc", resolve_region("auto")))
        );
        assert_eq!(
            translation_sidecar_for(base, "ja"),
            PathBuf::from("C:/music/Song.jp.lrc")
        );
    }
}

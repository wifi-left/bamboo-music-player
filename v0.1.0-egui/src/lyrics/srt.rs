//! SRT subtitle parser. Each block becomes one lyric line; extra text lines
//! are treated as translation. Basic HTML and `{\an8}` positioning tags are
//! stripped.

use std::path::PathBuf;

use super::{LyricLine, Lyrics, LyricsSource};

pub fn parse(
    text: &str,
    source: LyricsSource,
    path: Option<PathBuf>,
    encoding: Option<String>,
) -> Lyrics {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lyrics = Lyrics {
        source,
        path,
        encoding,
        ..Default::default()
    };

    for block in normalized.split("\n\n") {
        let lines: Vec<&str> = block
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        if lines.is_empty() {
            continue;
        }
        let Some(timing_index) = lines.iter().position(|line| line.contains("-->")) else {
            continue;
        };
        let Some((start_ms, end_ms)) = parse_timing(lines[timing_index]) else {
            continue;
        };
        let mut text_lines: Vec<String> = lines
            .iter()
            .skip(timing_index + 1)
            .map(|line| clean_text(line))
            .filter(|line| !line.is_empty())
            .collect();
        if text_lines.is_empty() {
            continue;
        }
        // A bilingual SRT block puts the translation on the following text
        // line(s). Keep them in one lyric line instead of treating each
        // same-timestamp line as a separate lyric.
        let text = text_lines.remove(0);
        let translation = (!text_lines.is_empty()).then(|| text_lines.join("\n"));
        lyrics.lines.push(LyricLine {
            start_ms,
            end_ms: Some(end_ms),
            text,
            translation,
            words: Vec::new(),
        });
    }

    lyrics.lines.sort_by_key(|line| line.start_ms);
    lyrics.timed = !lyrics.lines.is_empty();
    lyrics
}

fn parse_timing(line: &str) -> Option<(u64, u64)> {
    let (start, end) = line.split_once("-->")?;
    Some((parse_timestamp(start.trim())?, parse_timestamp(end.trim())?))
}

fn parse_timestamp(value: &str) -> Option<u64> {
    let value = value.split_whitespace().next()?;
    let parts: Vec<&str> = value.split(':').collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0u64, minutes.parse::<u64>().ok()?, *seconds),
        [hours, minutes, seconds] => (
            hours.parse::<u64>().ok()?,
            minutes.parse::<u64>().ok()?,
            *seconds,
        ),
        _ => return None,
    };
    let (secs, ms) = match seconds.split_once([',', '.']) {
        Some((secs, fraction)) => {
            let secs = secs.parse::<u64>().ok()?;
            let mut fraction = fraction.to_string();
            fraction.retain(|c| c.is_ascii_digit());
            while fraction.len() < 3 {
                fraction.push('0');
            }
            let ms = fraction[..3].parse::<u64>().ok()?;
            (secs, ms)
        }
        None => (seconds.parse::<u64>().ok()?, 0),
    };
    Some(((hours * 3600 + minutes * 60 + secs) * 1000) + ms)
}

fn clean_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '{' => {
                for next in chars.by_ref() {
                    if next == '}' {
                        break;
                    }
                }
            }
            '<' => {
                for next in chars.by_ref() {
                    if next == '>' {
                        break;
                    }
                }
            }
            _ => out.push(ch),
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bilingual_srt_and_cleans_tags() {
        let text = "\
1
00:00:01,000 --> 00:00:03,500
<i>Hello</i> world
你好世界

2
00:00:04.000 --> 00:00:06.000
{\\an8}Second line
";
        let lyrics = parse(text, LyricsSource::Sidecar, None, None);
        assert!(lyrics.timed);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].start_ms, 1000);
        assert_eq!(lyrics.lines[0].end_ms, Some(3500));
        assert_eq!(lyrics.lines[0].text, "Hello world");
        assert_eq!(lyrics.lines[0].translation.as_deref(), Some("你好世界"));
        assert_eq!(lyrics.index_at(1000), Some(0));
        assert_eq!(lyrics.lines[1].text, "Second line");
    }
}

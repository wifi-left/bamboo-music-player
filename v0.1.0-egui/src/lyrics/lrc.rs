//! LRC parser: metadata tags, multiple timestamps per line, `[offset:]` and
//! enhanced `<mm:ss.xx>` word timings. Lines sharing a timestamp are treated
//! as original + translation (the common bilingual LRC layout).

use super::{LyricLine, Lyrics, LyricsSource, WordTiming};

#[derive(Debug)]
struct Entry {
    start_ms: u64,
    text: String,
    words: Vec<WordTiming>,
}

pub fn parse(
    text: &str,
    source: LyricsSource,
    path: Option<std::path::PathBuf>,
    encoding: Option<String>,
) -> Lyrics {
    let mut lyrics = Lyrics {
        source,
        path,
        encoding,
        ..Default::default()
    };
    let mut entries: Vec<Entry> = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        let mut rest = line;
        let mut times: Vec<u64> = Vec::new();

        // Consume leading [tag] groups: time tags and metadata.
        loop {
            let Some(after_open) = rest.strip_prefix('[') else {
                break;
            };
            let Some(end) = after_open.find(']') else {
                break;
            };
            let tag = &after_open[..end];
            let after = after_open[end + 1..].trim_start();
            if let Some(ms) = parse_time_tag(tag) {
                times.push(ms);
                rest = after;
                continue;
            }
            if let Some((key, value)) = tag.split_once(':') {
                apply_meta(&mut lyrics, key.trim(), value.trim());
            }
            rest = after;
        }

        let (plain, words) = parse_enhanced(rest);
        let text_part = plain.trim().to_string();
        if times.is_empty() {
            if words.is_empty() || text_part.is_empty() {
                continue;
            }
            // Enhanced lyrics without a leading bracket time: use the first
            // word timestamp.
            times.push(words[0].start_ms);
        }
        for start_ms in times {
            entries.push(Entry {
                start_ms,
                text: text_part.clone(),
                words: words.clone(),
            });
        }
    }

    entries.sort_by_key(|entry| entry.start_ms);
    lyrics.timed = !entries.is_empty();

    // Keep every line, including several lines sharing a timestamp. The
    // stable sort preserves their file order, and the lookup returns the last
    // of an equal-timestamp group as the active line.
    for entry in entries {
        if entry.text.is_empty() && entry.words.is_empty() {
            continue;
        }
        lyrics.lines.push(LyricLine {
            start_ms: entry.start_ms,
            end_ms: None,
            text: entry.text,
            translation: None,
            words: entry.words,
        });
    }

    for index in 0..lyrics.lines.len() {
        let end = lyrics
            .lines
            .get(index + 1)
            .map(|next| next.start_ms)
            .filter(|end| *end > lyrics.lines[index].start_ms);
        lyrics.lines[index].end_ms = end;
    }
    lyrics
}

fn apply_meta(lyrics: &mut Lyrics, key: &str, value: &str) {
    match key.to_ascii_lowercase().as_str() {
        "ti" | "title" => lyrics.title = Some(value.to_string()),
        "ar" | "artist" => lyrics.artist = Some(value.to_string()),
        "al" | "album" => lyrics.album = Some(value.to_string()),
        "offset" => {
            if let Ok(offset) = value.trim().parse::<i64>() {
                lyrics.offset_ms = offset;
            }
        }
        _ => {}
    }
}

/// `mm:ss`, `mm:ss.xx`, `mm:ss.xxx`, or `hh:mm:ss.xx`.
pub fn parse_time_tag(tag: &str) -> Option<u64> {
    let tag = tag.trim();
    if tag.is_empty() {
        return None;
    }
    let parts: Vec<&str> = tag.split(':').collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0u64, minutes.parse::<u64>().ok()?, *seconds),
        [hours, minutes, seconds] => (
            hours.parse::<u64>().ok()?,
            minutes.parse::<u64>().ok()?,
            *seconds,
        ),
        _ => return None,
    };
    let (secs, fraction_ms) = match seconds.split_once(['.', ',']) {
        Some((secs, frac)) => {
            let secs = secs.parse::<u64>().ok()?;
            let mut frac = frac.trim().to_string();
            frac.retain(|c| c.is_ascii_digit());
            let ms = match frac.len() {
                0 => 0,
                1 => frac.parse::<u64>().ok()? * 100,
                2 => frac.parse::<u64>().ok()? * 10,
                _ => frac.parse::<u64>().ok()? / 10u64.pow((frac.len() - 3) as u32),
            };
            (secs, ms)
        }
        None => (seconds.parse::<u64>().ok()?, 0),
    };
    Some(((hours * 3600 + minutes * 60 + secs) * 1000) + fraction_ms)
}

/// Extract `<mm:ss.xx>word` timings. Returns plain text when there are no word
/// tags. The last word's end time is filled from the next word.
fn parse_enhanced(text: &str) -> (String, Vec<WordTiming>) {
    let mut words: Vec<WordTiming> = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    let mut pending: Option<(u64, String)> = None;

    loop {
        let Some(open) = rest.find('<') else {
            if let Some((start, mut word)) = pending {
                word.push_str(rest);
                if !word.trim().is_empty() {
                    words.push(WordTiming {
                        start_ms: start,
                        end_ms: None,
                        text: word,
                    });
                }
            } else {
                plain.push_str(rest);
            }
            break;
        };
        let segment = &rest[..open];
        if let Some((start, mut word)) = pending.take() {
            word.push_str(segment);
            words.push(WordTiming {
                start_ms: start,
                end_ms: None,
                text: word,
            });
        } else {
            plain.push_str(segment);
        }

        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('>') else {
            // Unmatched '<' is literal text.
            if let Some(last) = words.last_mut() {
                last.text.push('<');
                last.text.push_str(after_open);
            } else {
                plain.push('<');
                plain.push_str(after_open);
            }
            break;
        };
        let tag = &after_open[..close];
        rest = &after_open[close + 1..];
        if let Some(start_ms) = parse_time_tag(tag) {
            pending = Some((start_ms, String::new()));
        } else if let Some(last) = words.last_mut() {
            last.text.push('<');
            last.text.push_str(tag);
            last.text.push('>');
        } else {
            plain.push('<');
            plain.push_str(tag);
            plain.push('>');
        }
    }

    if words.is_empty() {
        return (plain, words);
    }
    for index in 0..words.len().saturating_sub(1) {
        words[index].end_ms = Some(words[index + 1].start_ms);
    }
    let plain: String = words.iter().map(|word| word.text.as_str()).collect();
    (plain, words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_times_offset_and_bilingual() {
        let text = "\
[ti:Test Song]
[ar:Singer]
[offset:500]
[00:01.00]first line
[00:01.00]第一行
[00:05.50][00:10.00]second line
";
        let lyrics = parse(text, LyricsSource::Sidecar, None, None);
        assert_eq!(lyrics.title.as_deref(), Some("Test Song"));
        assert_eq!(lyrics.artist.as_deref(), Some("Singer"));
        assert_eq!(lyrics.offset_ms, 500);
        assert!(lyrics.timed);
        assert_eq!(lyrics.lines.len(), 4);
        assert_eq!(lyrics.lines[0].start_ms, 1000);
        assert_eq!(lyrics.lines[0].text, "first line");
        assert_eq!(lyrics.lines[1].start_ms, 1000);
        assert_eq!(lyrics.lines[1].text, "第一行");
        // Equal timestamps keep file order and the last one is active.
        assert_eq!(lyrics.index_at(1000), Some(1));
        assert_eq!(lyrics.lines[2].start_ms, 5500);
        assert_eq!(lyrics.lines[3].start_ms, 10_000);
    }

    #[test]
    fn parses_enhanced_word_timings() {
        let text = "[00:02.00]<00:02.00>Hello <00:02.50>world<00:03.00>\n";
        let lyrics = parse(text, LyricsSource::Sidecar, None, None);
        assert_eq!(lyrics.lines.len(), 1);
        let line = &lyrics.lines[0];
        assert_eq!(line.text.trim(), "Hello world");
        assert_eq!(line.words.len(), 2);
        assert_eq!(line.words[0].start_ms, 2000);
        assert_eq!(line.words[0].end_ms, Some(2500));
        assert_eq!(line.words[1].start_ms, 2500);
    }

    #[test]
    fn duplicate_timestamps_keep_order_and_last_is_active() {
        let mut text = String::new();
        for second in 0..7 {
            let stamp = format!("[00:{:02}.00]", second * 5);
            text.push_str(&stamp);
            text.push_str(
                "line
",
            );
            text.push_str(&stamp);
            text.push_str(
                "翻译
",
            );
        }
        let lyrics = parse(&text, LyricsSource::Sidecar, None, None);
        assert_eq!(lyrics.lines.len(), 14);
        assert_eq!(lyrics.index_at(12_000), Some(5));
        assert_eq!(lyrics.lines[5].text, "翻译");
        assert_eq!(lyrics.index_at(26_000), Some(11));
    }

    #[test]
    fn index_at_honours_file_offset() {
        let mut lyrics = parse("[00:10.00]line\n", LyricsSource::Sidecar, None, None);
        lyrics.offset_ms = 500;
        assert_eq!(lyrics.index_at(9499), None);
        assert_eq!(lyrics.index_at(9500), Some(0));
    }
}

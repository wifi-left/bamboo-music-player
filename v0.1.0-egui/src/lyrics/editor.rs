//! Editable LRC document used by the lyrics editor.
//!
//! The editor keeps a flat list of lines with millisecond timestamps. Saving
//! serializes the list back to LRC and re-parses it through the normal parser,
//! so the player sees exactly the same model as a file loaded from disk.

use std::path::{Path, PathBuf};

use super::{LyricLine, Lyrics, LyricsSource, WordTiming};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditorLine {
    pub start_ms: u64,
    pub text: String,
    pub translation: Option<String>,
    /// Enhanced-LRC word timings. They survive save/load as long as the text
    /// itself is not edited.
    pub words: Vec<WordTiming>,
}

impl EditorLine {
    /// True when the word timings still describe exactly this line's text.
    pub fn words_match_text(&self) -> bool {
        if self.words.is_empty() {
            return false;
        }
        let joined: String = self.words.iter().map(|word| word.text.as_str()).collect();
        joined.trim_end() == self.text.trim_end()
            && self
                .words
                .windows(2)
                .all(|pair| pair[0].start_ms <= pair[1].start_ms)
    }

    fn enhanced_body(&self) -> Option<String> {
        if !self.words_match_text() {
            return None;
        }
        let mut out = String::new();
        for word in &self.words {
            if word.text.is_empty() {
                continue;
            }
            out.push('<');
            out.push_str(&format_tag(word.start_ms));
            out.push('>');
            out.push_str(&word.text);
        }
        Some(out)
    }
}

/// Document state captured for undo/redo.
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    lines: Vec<EditorLine>,
    selected: usize,
    timed: bool,
}

const MAX_HISTORY: usize = 100;
/// Consecutive keystrokes in the same text field collapse into one undo step.
const COALESCE_MS: u128 = 900;

#[derive(Clone, Debug)]
pub struct LyricsEditor {
    pub lines: Vec<EditorLine>,
    pub selected: usize,
    /// False when the source was plain text without timestamps.
    pub timed: bool,
    pub offset_ms: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub path: Option<PathBuf>,
    /// Audio file this document belongs to (used for the default save path).
    pub audio_path: Option<PathBuf>,
    pub track_key: String,
    pub track_label: String,
    /// Artist · album shown on the share card.
    pub track_subtitle: String,
    /// Encoded cover captured when the editor was opened.
    pub cover_png: Option<Vec<u8>>,
    pub dirty: bool,
    pub status: Option<String>,
    /// Buffer for the selected line's timestamp while it is being typed.
    pub time_text: String,
    pub text_text: String,
    /// Buffer for the selected line's translation.
    pub translation_text: String,
    /// Region written to `stem.<region>.lrc` when saving translations.
    pub translation_region: String,
    /// Translation sidecar this document was loaded from (if any).
    pub translation_path: Option<PathBuf>,
    pub confirm_close: bool,
    /// Amount used by the "shift all timestamps" control.
    pub shift_ms: i64,
    /// Spacing used when converting untimed lyrics.
    pub interval_ms: u64,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// State last written to disk; used to restore `dirty` after undo/redo.
    baseline: Snapshot,
    /// Last text field edit, for coalescing keystrokes.
    last_edit: Option<(usize, std::time::Instant)>,
}

impl LyricsEditor {
    pub fn from_lyrics(
        lyrics: Option<&Lyrics>,
        track_key: impl Into<String>,
        track_label: impl Into<String>,
        audio_path: Option<PathBuf>,
    ) -> Self {
        let mut editor = Self {
            lines: Vec::new(),
            selected: 0,
            timed: lyrics.map(|lyrics| lyrics.timed).unwrap_or(true),
            offset_ms: lyrics.map(|lyrics| lyrics.offset_ms).unwrap_or(0),
            title: lyrics.and_then(|lyrics| lyrics.title.clone()),
            artist: lyrics.and_then(|lyrics| lyrics.artist.clone()),
            album: lyrics.and_then(|lyrics| lyrics.album.clone()),
            path: lyrics.and_then(|lyrics| lyrics.path.clone()),
            audio_path,
            track_key: track_key.into(),
            track_label: track_label.into(),
            track_subtitle: String::new(),
            cover_png: None,
            dirty: false,
            status: None,
            time_text: String::new(),
            text_text: String::new(),
            translation_text: String::new(),
            translation_region: crate::lyrics::resolve_region("auto"),
            translation_path: lyrics.and_then(|lyrics| lyrics.translation_path.clone()),
            confirm_close: false,
            shift_ms: 0,
            interval_ms: 4_000,
            undo: Vec::new(),
            redo: Vec::new(),
            baseline: Snapshot {
                lines: Vec::new(),
                selected: 0,
                timed: true,
            },
            last_edit: None,
        };
        if let Some(lyrics) = lyrics {
            editor.lines = lyrics
                .lines
                .iter()
                .map(|line| EditorLine {
                    start_ms: line.start_ms,
                    text: line.text.clone(),
                    translation: line.translation.clone(),
                    words: line.words.clone(),
                })
                .collect();
        }
        if editor.lines.is_empty() {
            editor.lines.push(EditorLine::default());
        }
        editor.sync_buffers();
        editor.baseline = editor.snapshot();
        editor
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            selected: self.selected,
            timed: self.timed,
        }
    }

    /// Push the current document onto the undo stack unless a continuous text
    /// edit on the same line is still in progress.
    fn begin_edit(&mut self, coalesce_line: Option<usize>) {
        let now = std::time::Instant::now();
        let coalesce = match (coalesce_line, self.last_edit) {
            (Some(line), Some((last_line, at))) => {
                line == last_line && now.duration_since(at).as_millis() < COALESCE_MS
            }
            _ => false,
        };
        if !coalesce {
            self.undo.push(self.snapshot());
            if self.undo.len() > MAX_HISTORY {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = coalesce_line.map(|line| (line, now));
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.lines = snapshot.lines;
        self.selected = snapshot.selected.min(self.lines.len().saturating_sub(1));
        self.timed = snapshot.timed;
        self.sync_buffers();
        self.refresh_dirty();
    }

    /// Compare against the last saved document (selection is not content).
    fn refresh_dirty(&mut self) {
        let saved = &self.baseline;
        self.dirty = self.timed != saved.timed || self.lines != saved.lines;
    }

    /// Record that the document was written to disk, clearing `dirty`.
    pub fn mark_saved(&mut self) {
        self.dirty = false;
        self.baseline = self.snapshot();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) {
        let Some(previous) = self.undo.pop() else {
            return;
        };
        self.redo.push(self.snapshot());
        self.restore(previous);
        self.last_edit = None;
        self.status = None;
    }

    pub fn redo(&mut self) {
        let Some(next) = self.redo.pop() else {
            return;
        };
        self.undo.push(self.snapshot());
        self.restore(next);
        self.last_edit = None;
        self.status = None;
    }

    pub fn select(&mut self, index: usize) {
        if index < self.lines.len() {
            self.selected = index;
            self.sync_buffers();
        }
    }

    /// Refresh the text buffers after the selection changed.
    pub fn sync_buffers(&mut self) {
        if let Some(line) = self.lines.get(self.selected) {
            self.time_text = if self.timed {
                format_tag(line.start_ms)
            } else {
                String::new()
            };
            self.text_text = line.text.clone();
            self.translation_text = line.translation.clone().unwrap_or_default();
        } else {
            self.time_text.clear();
            self.text_text.clear();
            self.translation_text.clear();
        }
    }

    /// Apply the timestamp buffer to the selected line. Returns false when the
    /// text is not a valid timestamp.
    pub fn apply_time_text(&mut self) -> bool {
        let Some(ms) = parse_tag(&self.time_text) else {
            return false;
        };
        let changed = self
            .lines
            .get(self.selected)
            .is_some_and(|line| line.start_ms != ms || !self.timed);
        if changed {
            self.begin_edit(Some(self.selected));
            if let Some(line) = self.lines.get_mut(self.selected) {
                let delta = ms as i64 - line.start_ms as i64;
                line.start_ms = ms;
                shift_word_times(&mut line.words, delta);
            }
            self.timed = true;
            self.refresh_dirty();
        }
        true
    }

    /// Apply the text buffer to the selected line.
    pub fn apply_text_text(&mut self) {
        let changed = self
            .lines
            .get(self.selected)
            .is_some_and(|line| line.text != self.text_text);
        if changed {
            self.begin_edit(Some(self.selected));
            if let Some(line) = self.lines.get_mut(self.selected) {
                line.text = self.text_text.clone();
            }
            self.refresh_dirty();
        }
    }

    /// Apply the translation buffer to the selected line. Clearing the field
    /// removes the translation (and eventually its sidecar on save).
    pub fn apply_translation_text(&mut self) {
        let value = {
            let text = self.translation_text.trim();
            (!text.is_empty()).then(|| self.translation_text.clone())
        };
        let changed = self
            .lines
            .get(self.selected)
            .is_some_and(|line| line.translation != value);
        if changed {
            self.begin_edit(Some(self.selected));
            if let Some(line) = self.lines.get_mut(self.selected) {
                line.translation = value;
            }
            self.refresh_dirty();
        }
    }

    pub fn has_translations(&self) -> bool {
        self.lines.iter().any(|line| {
            line.translation
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
        })
    }

    /// Serialize only the translations, using the base document's timestamps.
    pub fn translation_to_lrc(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            let Some(translation) = line
                .translation
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
                continue;
            };
            if self.timed {
                out.push_str(&format!("[{}]{}\n", format_tag(line.start_ms), translation));
            } else {
                out.push_str(translation);
                out.push('\n');
            }
        }
        out
    }

    pub fn translation_sidecar_path(&self, base: &Path) -> PathBuf {
        crate::lyrics::translation_sidecar_for(base, &self.translation_region)
    }

    pub fn write_translation_to(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.translation_to_lrc())
    }
    pub fn stamp(&mut self, position_secs: f64) {
        let ms = (position_secs.max(0.0) * 1000.0).round() as u64;
        let changed = self
            .lines
            .get(self.selected)
            .is_some_and(|line| line.start_ms != ms || !self.timed);
        if changed {
            self.begin_edit(None);
            if let Some(line) = self.lines.get_mut(self.selected) {
                let delta = ms as i64 - line.start_ms as i64;
                line.start_ms = ms;
                shift_word_times(&mut line.words, delta);
            }
            self.timed = true;
            self.refresh_dirty();
        }
        self.time_text = format_tag(ms);
    }

    pub fn add_after_selected(&mut self, position_secs: f64) {
        let start_ms = self
            .lines
            .get(self.selected)
            .map(|line| {
                let next = self.lines.get(self.selected + 1).map(|line| line.start_ms);
                match next {
                    Some(next) if next > line.start_ms + 200 => (line.start_ms + next) / 2,
                    _ => line.start_ms.saturating_add(2000),
                }
            })
            .unwrap_or_else(|| (position_secs.max(0.0) * 1000.0) as u64);
        self.begin_edit(None);
        self.lines.insert(
            self.selected + 1,
            EditorLine {
                start_ms,
                text: String::new(),
                translation: None,
                words: Vec::new(),
            },
        );
        self.selected += 1;
        self.timed = self.timed || position_secs > 0.0;
        self.refresh_dirty();
        self.sync_buffers();
    }

    pub fn remove_selected(&mut self) {
        if self.lines.len() <= 1 {
            let changed = self
                .lines
                .get(0)
                .is_some_and(|line| !line.text.is_empty() || line.start_ms != 0);
            if changed {
                self.begin_edit(None);
                if let Some(line) = self.lines.get_mut(0) {
                    *line = EditorLine::default();
                }
                self.refresh_dirty();
            }
            self.selected = 0;
            self.sync_buffers();
            return;
        }
        if self.selected < self.lines.len() {
            self.begin_edit(None);
            self.lines.remove(self.selected);
            self.selected = self.selected.min(self.lines.len() - 1);
            self.refresh_dirty();
            self.sync_buffers();
        }
    }

    /// Move the selected line one position up or down in the file order.
    pub fn move_selected(&mut self, up: bool) {
        let index = self.selected;
        let target = if up {
            index.checked_sub(1)
        } else if index + 1 < self.lines.len() {
            Some(index + 1)
        } else {
            None
        };
        let Some(target) = target else {
            return;
        };
        self.begin_edit(None);
        self.lines.swap(index, target);
        self.selected = target;
        self.refresh_dirty();
        self.sync_buffers();
    }

    /// Sort the lines by timestamp (stable for equal timestamps).
    pub fn sort_by_time(&mut self) {
        if !self
            .lines
            .windows(2)
            .all(|pair| pair[0].start_ms <= pair[1].start_ms)
        {
            self.begin_edit(None);
            self.lines.sort_by_key(|line| line.start_ms);
            self.refresh_dirty();
            self.selected = self.selected.min(self.lines.len().saturating_sub(1));
            self.sync_buffers();
        }
    }

    /// Shift every timestamp by `delta_ms` (clamped at zero).
    pub fn shift_all(&mut self, delta_ms: i64) {
        if delta_ms == 0 {
            return;
        }
        self.begin_edit(None);
        for line in &mut self.lines {
            line.start_ms = shift_time(line.start_ms, delta_ms);
            shift_word_times(&mut line.words, delta_ms);
        }
        self.timed = true;
        self.refresh_dirty();
        self.sync_buffers();
    }

    /// Convert plain text into a timestamped document using evenly spaced
    /// lines. The first line starts at zero; the spacing defaults to four
    /// seconds, which gives the user a usable starting point for stamping.
    pub fn enable_timeline(&mut self, interval_ms: u64) {
        if self.timed {
            return;
        }
        self.begin_edit(None);
        let interval = interval_ms.max(500);
        for (index, line) in self.lines.iter_mut().enumerate() {
            line.start_ms = index as u64 * interval;
        }
        self.timed = true;
        self.refresh_dirty();
        self.sync_buffers();
    }

    pub fn to_lrc(&self) -> String {
        let mut out = String::new();
        if self.timed {
            if let Some(title) = self.title.as_ref().filter(|value| !value.trim().is_empty()) {
                out.push_str(&format!("[ti:{}]\n", title.trim()));
            }
            if let Some(artist) = self
                .artist
                .as_ref()
                .filter(|value| !value.trim().is_empty())
            {
                out.push_str(&format!("[ar:{}]\n", artist.trim()));
            }
            if let Some(album) = self.album.as_ref().filter(|value| !value.trim().is_empty()) {
                out.push_str(&format!("[al:{}]\n", album.trim()));
            }
            if self.offset_ms != 0 {
                out.push_str(&format!("[offset:{}]\n", self.offset_ms));
            }
        }
        for line in &self.lines {
            let text = line.text.trim_end();
            if text.trim().is_empty() {
                continue;
            }
            if self.timed {
                let body = line.enhanced_body().unwrap_or_else(|| text.to_string());
                out.push_str(&format!("[{}]{}\n", format_tag(line.start_ms), body));
            } else {
                out.push_str(text);
                out.push('\n');
            }
        }
        out
    }

    /// Serialize and parse the document through the normal lyrics parser.
    pub fn to_lyrics(&self, path: Option<PathBuf>) -> Lyrics {
        let text = self.to_lrc();
        let mut lyrics =
            super::parse_text(&text, LyricsSource::Edited, path, Some("UTF-8".to_string()));
        if lyrics.lines.is_empty() && !text.trim().is_empty() {
            // Fall back to plain text if the document somehow has no content
            // lines (for example only metadata tags).
            lyrics =
                super::parse_plain(&text, LyricsSource::Edited, None, Some("UTF-8".to_string()));
            lyrics.timed = false;
        }
        // `to_lrc` writes the original text only; translations live in the
        // sidecar and must be carried over to the live model after parsing.
        if lyrics.lines.len() == self.lines.len() {
            for (line, editor) in lyrics.lines.iter_mut().zip(self.lines.iter()) {
                line.translation = editor
                    .translation
                    .clone()
                    .filter(|text| !text.trim().is_empty());
            }
        } else {
            for line in &mut lyrics.lines {
                if let Some(editor) = self
                    .lines
                    .iter()
                    .find(|item| item.start_ms == line.start_ms)
                {
                    line.translation = editor
                        .translation
                        .clone()
                        .filter(|text| !text.trim().is_empty());
                }
            }
        }
        if self.has_translations() {
            lyrics.translation_path = self.translation_path.clone();
        }
        lyrics
    }

    /// Default save target: the loaded sidecar, otherwise the audio file's
    /// sibling `.lrc`.
    pub fn default_save_path(&self) -> Option<PathBuf> {
        if let Some(path) = &self.path {
            return Some(path.clone());
        }
        self.audio_path
            .as_ref()
            .map(|audio| audio.with_extension("lrc"))
    }

    /// True when the timestamps are not in ascending order.
    pub fn out_of_order(&self) -> bool {
        self.lines
            .windows(2)
            .any(|pair| pair[0].start_ms > pair[1].start_ms)
    }

    pub fn write_to(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.to_lrc())
    }
}

/// `[mm:ss.xx]`, `[mm:ss.xxx]` and `[hh:mm:ss.xx]` all parse.
pub fn parse_tag(text: &str) -> Option<u64> {
    let tag = text.trim().trim_start_matches('[').trim_end_matches(']');
    super::lrc::parse_time_tag(tag)
}

pub fn format_tag(ms: u64) -> String {
    let total_secs = ms / 1000;
    let centis = ((ms % 1000) + 5) / 10;
    let (total_secs, centis) = if centis >= 100 {
        (total_secs + 1, 0)
    } else {
        (total_secs, centis)
    };
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}.{centis:02}")
    } else {
        format!("{minutes:02}:{seconds:02}.{centis:02}")
    }
}

fn shift_time(ms: u64, delta: i64) -> u64 {
    if delta >= 0 {
        ms.saturating_add(delta as u64)
    } else {
        ms.saturating_sub((-delta) as u64)
    }
}

fn shift_word_times(words: &mut [WordTiming], delta: i64) {
    if delta == 0 {
        return;
    }
    for word in words {
        word.start_ms = shift_time(word.start_ms, delta);
        word.end_ms = word.end_ms.map(|end| shift_time(end, delta));
    }
}

impl From<&LyricLine> for EditorLine {
    fn from(line: &LyricLine) -> Self {
        Self {
            start_ms: line.start_ms,
            text: line.text.clone(),
            translation: line.translation.clone(),
            words: line.words.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Lyrics {
        super::super::parse_text(
            "[ti:Demo]\n[ar:Singer]\n[00:01.00]one\n[00:02.50]two\n",
            LyricsSource::Manual,
            None,
            None,
        )
    }

    #[test]
    fn formats_and_parses_timestamps() {
        assert_eq!(format_tag(0), "00:00.00");
        assert_eq!(format_tag(1_230), "00:01.23");
        assert_eq!(format_tag(61_000), "01:01.00");
        assert_eq!(format_tag(3_723_450), "01:02:03.45");
        assert_eq!(parse_tag("[00:01.23]"), Some(1_230));
        assert_eq!(parse_tag("01:02.50"), Some(62_500));
        assert_eq!(parse_tag("[01:02:03.45]"), Some(3_723_450));
        assert_eq!(parse_tag("nope"), None);
    }

    #[test]
    fn round_trips_through_the_parser() {
        let lyrics = sample();
        let editor = LyricsEditor::from_lyrics(Some(&lyrics), "key", "Demo", None);
        assert_eq!(editor.lines.len(), 2);
        assert!(editor.timed);

        let text = editor.to_lrc();
        assert!(text.contains("[ti:Demo]"));
        assert!(text.contains("[ar:Singer]"));
        assert!(text.contains("[00:01.00]one"));
        assert!(text.contains("[00:02.50]two"));

        let parsed = editor.to_lyrics(None);
        assert_eq!(parsed.lines.len(), 2);
        assert_eq!(parsed.lines[0].start_ms, 1_000);
        assert_eq!(parsed.lines[1].text, "two");
        assert_eq!(parsed.title.as_deref(), Some("Demo"));
        assert!(parsed.timed);
    }

    #[test]
    fn editing_stamping_and_ordering() {
        let lyrics = sample();
        let mut editor = LyricsEditor::from_lyrics(Some(&lyrics), "key", "Demo", None);
        editor.select(1);
        editor.time_text = "00:00.50".to_string();
        assert!(editor.apply_time_text());
        assert_eq!(editor.lines[1].start_ms, 500);
        assert!(editor.out_of_order());

        editor.sort_by_time();
        assert!(!editor.out_of_order());
        assert_eq!(editor.lines[0].start_ms, 500);
        assert_eq!(editor.lines[1].start_ms, 1_000);

        editor.select(1);
        editor.time_text = "00:05.00".to_string();
        assert!(editor.apply_time_text());
        assert_eq!(editor.lines[1].start_ms, 5_000);

        editor.select(0);
        editor.stamp(2.25);
        assert_eq!(editor.lines[0].start_ms, 2_250);

        editor.select(0);
        editor.add_after_selected(3.0);
        assert_eq!(editor.lines.len(), 3);
        assert_eq!(editor.selected, 1);
        assert_eq!(editor.lines[1].start_ms, 3_625);
        editor.text_text = "middle".to_string();
        editor.apply_text_text();
        assert_eq!(editor.lines[1].text, "middle");

        editor.shift_all(500);
        assert_eq!(editor.lines[1].start_ms, 4_125);

        editor.remove_selected();
        assert_eq!(editor.lines.len(), 2);
    }

    #[test]
    fn plain_text_round_trip_stays_untimed() {
        let mut editor = LyricsEditor::from_lyrics(None, "key", "Demo", None);
        editor.lines = vec![
            EditorLine {
                start_ms: 0,
                text: "alpha".to_string(),
                translation: None,
                words: Vec::new(),
            },
            EditorLine {
                start_ms: 0,
                text: "beta".to_string(),
                translation: None,
                words: Vec::new(),
            },
        ];
        editor.timed = false;
        let text = editor.to_lrc();
        assert_eq!(text, "alpha\nbeta\n");
        let parsed = editor.to_lyrics(None);
        assert!(!parsed.timed);
        assert_eq!(parsed.lines.len(), 2);

        editor.enable_timeline(2_000);
        assert!(editor.timed);
        assert_eq!(editor.lines[1].start_ms, 2_000);
        let parsed = editor.to_lyrics(None);
        assert!(parsed.timed);
        assert_eq!(parsed.lines[1].start_ms, 2_000);
    }

    #[test]
    fn undo_and_redo_restore_document_state() {
        let lyrics = sample();
        let mut editor = LyricsEditor::from_lyrics(Some(&lyrics), "key", "Demo", None);
        assert!(!editor.can_undo());
        editor.select(0);
        editor.stamp(5.0);
        assert_eq!(editor.lines[0].start_ms, 5_000);
        assert!(editor.dirty);
        assert!(editor.can_undo());

        editor.undo();
        assert_eq!(editor.lines[0].start_ms, 1_000);
        assert!(!editor.can_undo());
        assert!(editor.can_redo());
        assert!(
            !editor.dirty,
            "undoing back to the loaded state clears dirty"
        );

        editor.redo();
        assert_eq!(editor.lines[0].start_ms, 5_000);
        assert!(editor.dirty);
        assert!(!editor.can_redo());
    }

    #[test]
    fn undo_covers_structural_edits() {
        let lyrics = sample();
        let mut editor = LyricsEditor::from_lyrics(Some(&lyrics), "key", "Demo", None);
        editor.select(0);
        editor.add_after_selected(3.0);
        assert_eq!(editor.lines.len(), 3);
        editor.remove_selected();
        assert_eq!(editor.lines.len(), 2);

        editor.undo();
        assert_eq!(editor.lines.len(), 3, "undo restores the removed line");
        editor.undo();
        assert_eq!(editor.lines.len(), 2, "undo removes the added line");
    }

    #[test]
    fn continuous_typing_is_one_undo_step() {
        let lyrics = sample();
        let mut editor = LyricsEditor::from_lyrics(Some(&lyrics), "key", "Demo", None);
        editor.select(0);
        editor.text_text = "first".to_string();
        editor.apply_text_text();
        editor.text_text = "first!".to_string();
        editor.apply_text_text();
        assert_eq!(editor.lines[0].text, "first!");
        assert_eq!(editor.undo.len(), 1, "keystrokes are coalesced");

        editor.undo();
        assert_eq!(editor.lines[0].text, "one");
    }

    #[test]
    fn saved_baseline_is_restored_after_undo() {
        let lyrics = sample();
        let mut editor = LyricsEditor::from_lyrics(Some(&lyrics), "key", "Demo", None);
        editor.select(0);
        editor.stamp(5.0);
        editor.mark_saved();
        assert!(!editor.dirty);
        editor.stamp(6.0);
        assert!(editor.dirty);
        editor.undo();
        assert_eq!(editor.lines[0].start_ms, 5_000);
        assert!(!editor.dirty);
    }

    #[test]
    fn preserves_word_timings_until_text_changes() {
        let lyrics = super::super::parse_text(
            "[00:01.00]<00:01.00>Hello <00:01.50>world\n",
            LyricsSource::Manual,
            None,
            None,
        );
        assert_eq!(lyrics.lines[0].words.len(), 2);
        let mut editor = LyricsEditor::from_lyrics(Some(&lyrics), "key", "Demo", None);
        let text = editor.to_lrc();
        assert!(
            text.contains("[00:01.00]<00:01.00>Hello <00:01.50>world"),
            "{text}"
        );
        let parsed = editor.to_lyrics(None);
        assert_eq!(parsed.lines[0].text, "Hello world");
        assert_eq!(parsed.lines[0].words.len(), 2);

        editor.text_text = "Hello there".to_string();
        editor.apply_text_text();
        assert!(!editor.lines[0].words_match_text());
        let text = editor.to_lrc();
        assert!(!text.contains('<'), "{text}");
    }

    #[test]
    fn word_timings_follow_line_and_global_time_shifts() {
        let lyrics = super::super::parse_text(
            "[00:01.00]<00:01.00>Hello <00:01.50>world\n",
            LyricsSource::Manual,
            None,
            None,
        );
        let mut editor = LyricsEditor::from_lyrics(Some(&lyrics), "key", "Demo", None);
        editor.select(0);
        editor.time_text = "00:05.00".to_string();
        assert!(editor.apply_time_text());
        assert_eq!(editor.lines[0].words[0].start_ms, 5_000);
        assert_eq!(editor.lines[0].words[1].start_ms, 5_500);

        editor.shift_all(-1_000);
        assert_eq!(editor.lines[0].start_ms, 4_000);
        assert_eq!(editor.lines[0].words[0].start_ms, 4_000);
        assert_eq!(editor.lines[0].words[1].start_ms, 4_500);
    }

    #[test]
    fn default_save_path_prefers_sidecar_then_audio() {
        let mut editor = LyricsEditor::from_lyrics(None, "key", "Demo", None);
        assert_eq!(editor.default_save_path(), None);
        editor.audio_path = Some(PathBuf::from("C:/music/song.mp3"));
        assert_eq!(
            editor.default_save_path(),
            Some(PathBuf::from("C:/music/song.lrc"))
        );
        editor.path = Some(PathBuf::from("C:/music/custom.lrc"));
        assert_eq!(
            editor.default_save_path(),
            Some(PathBuf::from("C:/music/custom.lrc"))
        );
    }

    #[test]
    fn translations_are_editable_and_round_trip() {
        let lyrics = sample();
        let mut editor = LyricsEditor::from_lyrics(Some(&lyrics), "key", "Demo", None);
        editor.select(0);
        editor.translation_text = "第一行".to_string();
        editor.apply_translation_text();
        assert!(editor.dirty);
        assert!(editor.has_translations());
        assert_eq!(editor.lines[0].translation.as_deref(), Some("第一行"));

        // The base LRC stays original-only; translations go to the sidecar.
        let base = editor.to_lrc();
        assert!(!base.contains("第一行"), "{base}");
        let sidecar = editor.translation_to_lrc();
        assert!(sidecar.contains("[00:01.00]第一行"), "{sidecar}");
        assert!(!sidecar.contains("one"), "{sidecar}");

        // The live model still carries the translation after parsing.
        let parsed = editor.to_lyrics(None);
        assert_eq!(parsed.lines[0].translation.as_deref(), Some("第一行"));

        // Clearing a translation marks the document dirty again.
        editor.select(0);
        editor.translation_text.clear();
        editor.apply_translation_text();
        assert!(!editor.has_translations());
        let parsed = editor.to_lyrics(None);
        assert!(parsed.lines[0].translation.is_none());
    }

    #[test]
    fn translation_region_controls_sidecar_name() {
        let mut editor = LyricsEditor::from_lyrics(None, "key", "Demo", None);
        editor.translation_region = "jp".to_string();
        assert_eq!(
            editor.translation_sidecar_path(Path::new("C:/music/song.lrc")),
            PathBuf::from("C:/music/song.jp.lrc")
        );
        editor.translation_region = "en".to_string();
        assert_eq!(
            editor.translation_sidecar_path(Path::new("C:/music/song.srt")),
            PathBuf::from("C:/music/song.en.lrc")
        );
    }
}

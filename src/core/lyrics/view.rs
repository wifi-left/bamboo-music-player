//! UI-independent layout for the scrolling lyric list.
//!
//! The main window, the standalone lyric window, the desktop lyric overlay and
//! the mini player all need the same answers: where each line sits, which line
//! is current, how far to scroll to center it, and which line a click hits.
//! Rendering stays with each surface; this module is pure geometry.
//!
//! Inputs are a [`Lyrics`] document plus a viewport height; nothing here knows
//! about DPI, fonts, windows or painting.

use std::ops::Range;

use super::{LyricLine, Lyrics};

/// Height of the primary text row in DIPs.
pub const DEFAULT_LINE_HEIGHT: f32 = 30.0;
/// Extra height taken by a translation row when one is present and enabled.
pub const DEFAULT_TRANSLATION_HEIGHT: f32 = 20.0;
/// Vertical gap between two lyric entries.
pub const DEFAULT_LINE_GAP: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewConfig {
    pub line_height: f32,
    pub translation_height: f32,
    pub gap: f32,
    /// Whether translation rows participate in the layout at all. When false
    /// the translation is skipped completely instead of leaving a blank row.
    pub show_translation: bool,
}

impl Default for ViewConfig {
    fn default() -> Self {
        Self {
            line_height: DEFAULT_LINE_HEIGHT,
            translation_height: DEFAULT_TRANSLATION_HEIGHT,
            gap: DEFAULT_LINE_GAP,
            show_translation: true,
        }
    }
}

/// Geometry of one lyric entry in content coordinates (0 = first line top).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineMetrics {
    pub top: f32,
    pub height: f32,
    pub text_height: f32,
    pub translation_height: f32,
}

impl LineMetrics {
    pub fn bottom(&self) -> f32 {
        self.top + self.height
    }

    pub fn contains(&self, y: f32) -> bool {
        y >= self.top && y < self.top + self.height
    }
}

/// Row geometry for the current [`Lyrics`] document.
#[derive(Clone, Debug, Default)]
pub struct LyricsView {
    config: ViewConfig,
    lines: Vec<LineMetrics>,
    total: f32,
}

impl LyricsView {
    pub fn new(config: ViewConfig) -> Self {
        Self {
            config,
            lines: Vec::new(),
            total: 0.0,
        }
    }

    pub fn config(&self) -> ViewConfig {
        self.config
    }

    /// Replace the layout configuration. Returns true when it changed, so the
    /// caller knows it must rebuild and repaint.
    pub fn set_config(&mut self, config: ViewConfig) -> bool {
        if self.config == config {
            return false;
        }
        self.config = config;
        true
    }

    /// Rebuild the row geometry. Returns true when the resulting layout differs
    /// from the previous one (the caller can then invalidate the whole list).
    pub fn rebuild(&mut self, lyrics: Option<&Lyrics>) -> bool {
        let mut lines = Vec::new();
        let mut cursor = 0.0f32;
        if let Some(lyrics) = lyrics {
            lines.reserve(lyrics.lines.len());
            for line in &lyrics.lines {
                let text_height = self.config.line_height.max(1.0);
                let translation_height = if self.config.show_translation
                    && line
                        .translation
                        .as_deref()
                        .is_some_and(|translation| !translation.trim().is_empty())
                {
                    self.config.translation_height.max(0.0)
                } else {
                    0.0
                };
                let height = text_height + translation_height;
                lines.push(LineMetrics {
                    top: cursor,
                    height,
                    text_height,
                    translation_height,
                });
                cursor += height + self.config.gap.max(0.0);
            }
        }
        let total = if lines.is_empty() {
            0.0
        } else {
            (cursor - self.config.gap.max(0.0)).max(0.0)
        };
        if lines == self.lines && (total - self.total).abs() < 0.01 {
            self.total = total;
            return false;
        }
        self.lines = lines;
        self.total = total;
        true
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn total_height(&self) -> f32 {
        self.total
    }

    pub fn line(&self, index: usize) -> Option<LineMetrics> {
        self.lines.get(index).copied()
    }

    /// Smallest valid scroll offset for `viewport` DIPs. Negative values push
    /// the first line down so it can sit in the vertical center.
    pub fn min_scroll(&self, viewport: f32) -> f32 {
        let Some(first) = self.lines.first() else {
            return 0.0;
        };
        (first.top + first.height * 0.5 - viewport * 0.5).min(0.0)
    }

    /// Largest valid scroll offset: the last line can be centered too.
    pub fn max_scroll(&self, viewport: f32) -> f32 {
        let Some(last) = self.lines.last() else {
            return 0.0;
        };
        (last.top + last.height * 0.5 - viewport * 0.5).max(0.0)
    }

    /// Scroll offset that centers `index` in the viewport (unclamped).
    pub fn scroll_for_line(&self, index: usize, viewport: f32) -> f32 {
        let Some(line) = self.lines.get(index) else {
            return 0.0;
        };
        line.top + line.height * 0.5 - viewport * 0.5
    }

    /// Scroll offset that centers `index`, clamped so first/last lines can
    /// still reach the middle instead of sticking to an edge.
    pub fn centered_scroll(&self, index: usize, viewport: f32) -> f32 {
        self.scroll_for_line(index, viewport)
            .clamp(self.min_scroll(viewport), self.max_scroll(viewport))
    }

    /// Indices whose rows intersect `[scroll, scroll + viewport)`.
    pub fn visible_range(&self, scroll: f32, viewport: f32) -> Range<usize> {
        if self.lines.is_empty() || viewport <= 0.0 {
            return 0..0;
        }
        let top = scroll;
        let bottom = top + viewport;
        let first = self
            .lines
            .partition_point(|line| line.top + line.height <= top);
        let mut end = first;
        while end < self.lines.len() && self.lines[end].top < bottom {
            end += 1;
        }
        first..end
    }

    /// Row under the cursor. `y` is in viewport coordinates; gaps between rows
    /// return `None` so hovering empty space does not highlight a line.
    pub fn hit_test(&self, scroll: f32, viewport: f32, y: f32) -> Option<usize> {
        if y < 0.0 || y >= viewport {
            return None;
        }
        let content_y = y + scroll;
        let candidate = self.lines.partition_point(|line| line.top <= content_y);
        let index = candidate.checked_sub(1)?;
        let line = self.lines.get(index)?;
        line.contains(content_y).then_some(index)
    }
}

/// Playback position with the document's offset applied.
pub fn adjusted_position(lyrics: &Lyrics, position_ms: u64) -> u64 {
    (position_ms as i64 + lyrics.offset_ms).max(0) as u64
}

/// End of `index` in media time: the explicit end, the next line's start, or a
/// four-second fallback so the last line still has karaoke progress.
pub fn line_end_ms(lyrics: &Lyrics, index: usize) -> u64 {
    let Some(line) = lyrics.lines.get(index) else {
        return 0;
    };
    line.end_ms
        .or_else(|| lyrics.lines.get(index + 1).map(|next| next.start_ms))
        .unwrap_or(line.start_ms + 4_000)
        .max(line.start_ms + 1)
}

/// Progress through one line in `0.0..=1.0` (whole-line, not word aware).
pub fn line_progress(lyrics: &Lyrics, index: usize, position_ms: u64) -> f32 {
    if !lyrics.timed {
        return 0.0;
    }
    let Some(line) = lyrics.lines.get(index) else {
        return 0.0;
    };
    let position = adjusted_position(lyrics, position_ms);
    let start = line.start_ms as f32;
    let span = (line_end_ms(lyrics, index) as f32 - start).max(1.0);
    ((position as f32 - start) / span).clamp(0.0, 1.0)
}

/// Current line and its progress. `None` for untimed lyrics or before the
/// first timestamp.
pub fn current_line(lyrics: &Lyrics, position_ms: u64) -> Option<(usize, f32)> {
    let index = lyrics.index_at(position_ms)?;
    Some((index, line_progress(lyrics, index, position_ms)))
}

/// Plain text of one entry; kept here so every surface trims identically.
pub fn line_text(line: &LyricLine) -> &str {
    line.text.trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::{LyricLine, Lyrics, LyricsSource};

    fn lyric(start: u64, text: &str, translation: Option<&str>) -> LyricLine {
        LyricLine {
            start_ms: start,
            end_ms: None,
            text: text.to_string(),
            translation: translation.map(str::to_string),
            words: Vec::new(),
        }
    }

    fn document(lines: Vec<LyricLine>) -> Lyrics {
        Lyrics {
            lines,
            source: LyricsSource::Sidecar,
            timed: true,
            ..Default::default()
        }
    }

    #[test]
    fn translation_rows_add_height_only_when_present() {
        let lyrics = document(vec![
            lyric(0, "one", None),
            lyric(1_000, "two", Some("二")),
            lyric(2_000, "three", Some("   ")),
        ]);
        let mut view = LyricsView::new(ViewConfig::default());
        assert!(view.rebuild(Some(&lyrics)));
        assert_eq!(view.len(), 3);

        let first = view.line(0).unwrap();
        assert_eq!(first.height, DEFAULT_LINE_HEIGHT);
        assert_eq!(first.translation_height, 0.0);

        let second = view.line(1).unwrap();
        assert_eq!(
            second.height,
            DEFAULT_LINE_HEIGHT + DEFAULT_TRANSLATION_HEIGHT
        );
        assert_eq!(
            second.top,
            first.top + first.height + DEFAULT_LINE_GAP
        );

        // A blank translation must not leave a blank row.
        assert_eq!(view.line(2).unwrap().translation_height, 0.0);
        assert_eq!(
            view.total_height(),
            DEFAULT_LINE_HEIGHT * 3.0 + DEFAULT_TRANSLATION_HEIGHT + DEFAULT_LINE_GAP * 2.0
        );

        // Rebuilding the same document reports no change.
        assert!(!view.rebuild(Some(&lyrics)));
    }

    #[test]
    fn translation_can_be_disabled() {
        let lyrics = document(vec![lyric(0, "one", Some("一"))]);
        let mut view = LyricsView::new(ViewConfig {
            show_translation: false,
            ..Default::default()
        });
        view.rebuild(Some(&lyrics));
        assert_eq!(view.line(0).unwrap().height, DEFAULT_LINE_HEIGHT);
        assert_eq!(view.total_height(), DEFAULT_LINE_HEIGHT);
    }

    #[test]
    fn hit_test_ignores_gaps_and_outside_points() {
        let lyrics = document(vec![lyric(0, "one", None), lyric(1_000, "two", None)]);
        let mut view = LyricsView::new(ViewConfig::default());
        view.rebuild(Some(&lyrics));
        let viewport = 200.0;

        assert_eq!(view.hit_test(0.0, viewport, 0.0), Some(0));
        assert_eq!(view.hit_test(0.0, viewport, DEFAULT_LINE_HEIGHT - 0.1), Some(0));
        // Gap between rows.
        assert_eq!(view.hit_test(0.0, viewport, DEFAULT_LINE_HEIGHT + 1.0), None);
        let second_top = DEFAULT_LINE_HEIGHT + DEFAULT_LINE_GAP;
        assert_eq!(view.hit_test(0.0, viewport, second_top + 1.0), Some(1));
        assert_eq!(view.hit_test(0.0, viewport, -1.0), None);
        assert_eq!(view.hit_test(0.0, viewport, viewport), None);

        // Scrolled: a point at the top maps into the scrollable content.
        assert_eq!(view.hit_test(second_top, viewport, 1.0), Some(1));
    }

    #[test]
    fn scroll_centers_and_clamps() {
        let lines = (0..20)
            .map(|index| lyric(index as u64 * 1_000, "line", None))
            .collect();
        let lyrics = document(lines);
        let mut view = LyricsView::new(ViewConfig::default());
        view.rebuild(Some(&lyrics));
        let viewport = 120.0;

        let centered = view.centered_scroll(10, viewport);
        let line = view.line(10).unwrap();
        assert!(line.top < centered + viewport);
        assert!((line.top + line.height * 0.5 - (centered + viewport * 0.5)).abs() < 0.01);

        // First and last lines center instead of sticking to an edge: the
        // scroll range extends half a viewport beyond the content.
        let first = view.centered_scroll(0, viewport);
        assert!((first + viewport * 0.5 - view.line(0).unwrap().height * 0.5).abs() < 0.01);
        assert!(first < 0.0);
        let last = view.centered_scroll(19, viewport);
        let last_line = view.line(19).unwrap();
        assert!(
            (last + viewport * 0.5 - last_line.top - last_line.height * 0.5).abs() < 0.01
        );
        assert_eq!(last, view.max_scroll(viewport));
    }

    #[test]
    fn visible_range_covers_partial_rows() {
        let lines = (0..10)
            .map(|index| lyric(index as u64 * 1_000, "line", None))
            .collect();
        let lyrics = document(lines);
        let mut view = LyricsView::new(ViewConfig::default());
        view.rebuild(Some(&lyrics));

        let range = view.visible_range(0.0, DEFAULT_LINE_HEIGHT);
        assert_eq!(range.start, 0);
        assert_eq!(range.end, 1);

        let range = view.visible_range(DEFAULT_LINE_HEIGHT + 1.0, DEFAULT_LINE_HEIGHT);
        assert_eq!(range.start, 1);
        assert_eq!(range.end, 2);

        assert_eq!(view.visible_range(0.0, 0.0), 0..0);
    }

    #[test]
    fn current_line_respects_offset_and_gaps() {
        let mut lyrics = document(vec![
            lyric(1_000, "one", None),
            lyric(2_000, "two", None),
        ]);
        assert_eq!(current_line(&lyrics, 0), None);
        assert_eq!(current_line(&lyrics, 1_500).unwrap().0, 0);
        assert!((current_line(&lyrics, 1_500).unwrap().1 - 0.5).abs() < 0.01);

        // +500 ms offset makes media position 1_000 look like 1_500.
        lyrics.offset_ms = 500;
        assert_eq!(current_line(&lyrics, 1_000).unwrap().0, 0);

        // Negative offset clamps at zero.
        lyrics.offset_ms = -5_000;
        assert_eq!(adjusted_position(&lyrics, 1_000), 0);
    }

    #[test]
    fn untimed_lyrics_have_rows_but_no_current_line() {
        let mut lyrics = document(vec![lyric(0, "a", None), lyric(0, "b", None)]);
        lyrics.timed = false;
        let mut view = LyricsView::new(ViewConfig::default());
        view.rebuild(Some(&lyrics));
        assert_eq!(view.len(), 2);
        assert_eq!(current_line(&lyrics, 10_000), None);
        assert_eq!(line_progress(&lyrics, 0, 10_000), 0.0);
    }
}

//! Lyrics model. Parsing (LRC/SRT/encoding) is implemented in the lyrics
//! stage; this module keeps the shared types stable for the UI.

use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub struct WordTiming {
    pub start_ms: u64,
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
    pub encoding: Option<String>,
    pub raw: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LyricsSource {
    #[default]
    None,
    Sidecar,
    Embedded,
    Bamboo,
    Manual,
}

impl Lyrics {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Binary search for the line active at `position_ms`.
    pub fn index_at(&self, position_ms: u64) -> Option<usize> {
        if self.lines.is_empty() {
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

    pub fn line_at(&self, position_ms: u64) -> Option<&LyricLine> {
        self.index_at(position_ms).and_then(|i| self.lines.get(i))
    }
}

pub fn parse_loaded(_bytes: &[u8]) -> Lyrics {
    Lyrics::default()
}

//! Shared data model: tracks, sources, and small enums used across modules.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrackSource {
    Local {
        path: PathBuf,
    },
    Url {
        url: String,
    },
    Bamboo {
        source_id: String,
        song_id: String,
        base_url: String,
    },
    Radio {
        url: String,
    },
}

impl Default for TrackSource {
    fn default() -> Self {
        Self::Local {
            path: PathBuf::new(),
        }
    }
}

impl TrackSource {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Local { path } => Some(path),
            _ => None,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Local { .. } => "local",
            Self::Url { .. } => "url",
            Self::Bamboo { .. } => "bamboo",
            Self::Radio { .. } => "radio",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Track {
    pub source: TrackSource,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_id: Option<String>,
    pub artist_id: Option<String>,
    /// Duration in seconds when known (tag or API metadata).
    pub duration: Option<f64>,
    pub cover_url: Option<String>,
    pub source_name: String,
    pub track_no: Option<u32>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub lossless: bool,
    pub warning: Option<String>,
}

impl Track {
    pub fn local(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let (artist, title) = split_artist_title(&stem);
        let lossless = path
            .extension()
            .map(|e| {
                let e = e.to_string_lossy().to_ascii_lowercase();
                matches!(e.as_str(), "flac" | "wav" | "aiff" | "alac")
            })
            .unwrap_or(false);
        Self {
            source: TrackSource::Local { path },
            title,
            artist,
            source_name: "本地".to_string(),
            lossless,
            ..Default::default()
        }
    }

    pub fn display_title(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.clone();
        }
        match &self.source {
            TrackSource::Local { path } => path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| crate::i18n::tr("未知曲目").to_string()),
            TrackSource::Url { url } | TrackSource::Radio { url } => url.clone(),
            TrackSource::Bamboo { song_id, .. } => song_id.clone(),
        }
    }

    pub fn display_artist(&self) -> String {
        if self.artist.trim().is_empty() {
            crate::i18n::tr("未知歌手").to_string()
        } else {
            self.artist.clone()
        }
    }

    /// Fill artist/title from "Artist - Title" file names when tags are absent.
    pub fn apply_filename_fallback(&mut self) {
        if !self.artist.trim().is_empty() {
            return;
        }
        let Some(path) = self.source.path() else {
            return;
        };
        let Some(stem) = path.file_stem() else {
            return;
        };
        let stem = stem.to_string_lossy();
        let (artist, title) = split_artist_title(&stem);
        if artist.is_empty() {
            return;
        }
        self.artist = artist;
        if self.title.trim().is_empty() || self.title == stem {
            self.title = title;
        }
    }

    pub fn subtitle(&self) -> String {
        let artist = self.display_artist();
        if self.album.trim().is_empty() {
            artist
        } else {
            format!("{} · {}", artist, self.album)
        }
    }

    /// Case-insensitive match used by the queue filter. Empty queries match
    /// every track.
    pub fn matches_query(&self, query: &str) -> bool {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        [
            self.display_title(),
            self.artist.clone(),
            self.album.clone(),
            self.source_name.clone(),
        ]
        .iter()
        .any(|field| field.to_lowercase().contains(&needle))
    }

    /// Stable key used for de-duplication across queue/playlists.
    pub fn dedupe_key(&self) -> String {
        match &self.source {
            TrackSource::Local { path } => normalize_path(path),
            TrackSource::Url { url } | TrackSource::Radio { url } => {
                format!("url:{}", url.trim())
            }
            TrackSource::Bamboo {
                source_id, song_id, ..
            } => format!("bamboo:{}:{}", source_id, song_id),
        }
    }

    pub fn is_playable_local(&self) -> bool {
        match &self.source {
            TrackSource::Local { path } => path.is_file(),
            _ => true,
        }
    }
}

/// "Artist - Title" file names are common in the target library; use them as
/// a fallback when tags are missing.
fn split_artist_title(stem: &str) -> (String, String) {
    if let Some((artist, title)) = stem.split_once(" - ") {
        let artist = artist.trim();
        let title = title.trim();
        if !artist.is_empty() && !title.is_empty() {
            return (artist.to_string(), title.to_string());
        }
    }
    (String::new(), stem.to_string())
}

/// Case-insensitive, slash-normalized path key on Windows.
pub fn normalize_path(path: &Path) -> String {
    let s = path.to_string_lossy().replace('/', "\\");
    format!("local:{}", s.to_lowercase())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => crate::i18n::tr("顺序播放"),
            Self::All => crate::i18n::tr("列表循环"),
            Self::One => crate::i18n::tr("单曲循环"),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Page {
    #[default]
    NowPlaying,
    Library,
    Playlists,
    Search,
    Discover,
    Settings,
}

impl Page {
    pub const ALL: [Page; 6] = [
        Page::NowPlaying,
        Page::Library,
        Page::Playlists,
        Page::Search,
        Page::Discover,
        Page::Settings,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_query_matches_title_artist_album_and_source() {
        let mut track = Track::local("C:/music/song.mp3");
        track.title = "Midnight City".to_string();
        track.artist = "M83".to_string();
        track.album = "Hurry Up".to_string();
        track.source_name = "本地".to_string();
        assert!(track.matches_query(""));
        assert!(track.matches_query("midnight"));
        assert!(track.matches_query("  M83 "));
        assert!(track.matches_query("hurry"));
        assert!(!track.matches_query("jazz"));
    }
}

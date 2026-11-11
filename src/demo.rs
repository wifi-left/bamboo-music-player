//! Synthetic fixtures for `--bench`/`--self-test` only.
//!
//! The real UI paints [`crate::core::app::AppCore`] snapshots. These helpers
//! provide 10k rows and a timed lyric so painting cost can be measured without
//! a local music library and without touching the audio engine.

use std::path::PathBuf;

use crate::core::lyrics::{LyricLine, Lyrics, LyricsSource};
use crate::core::model::{Track, TrackSource};

const ARTISTS: &[&str] = &[
    "Aoi Teshima",
    "Hans Zimmer",
    "坂本龍一",
    "惘闻",
    "Nujabes",
    "陈绮贞",
    "Daughter",
    "堀江由衣",
    "Ólafur Arnalds",
    "李志",
];

const ALBUMS: &[&str] = &[
    "Dirty Redraw",
    "Native Shell",
    "Windows & Bamboo",
    "Late Night Queue",
    "Only Visible Rows",
    "Idle Is Zero",
];

pub fn core_tracks(count: usize) -> Vec<Track> {
    (0..count)
        .map(|index| Track {
            source: TrackSource::Local {
                path: PathBuf::from(format!("C:/bench/track-{:05}.mp3", index + 1)),
            },
            title: format!("Track {:05}", index + 1),
            artist: ARTISTS[index % ARTISTS.len()].to_string(),
            album: ALBUMS[index % ALBUMS.len()].to_string(),
            duration: Some(150.0 + (index % 90) as f64),
            source_name: "bench".to_string(),
            ..Default::default()
        })
        .collect()
}

const LYRICS: &[&str] = &[
    "夜色漫过窗台",
    "Bamboo player, native shell",
    "只重绘变化的矩形",
    "Idle frames are zero",
    "进度条只失效填充边缘",
    "Every line is virtualized",
    "让 KTV 跟着时间轴前进",
    "Direct2D paints the band",
    "暂停时没有周期重绘",
    "Ten thousand rows, one window",
];

pub fn core_lyrics() -> Lyrics {
    let lines = LYRICS
        .iter()
        .enumerate()
        .map(|(index, text)| LyricLine {
            start_ms: index as u64 * 12_000,
            end_ms: Some((index as u64 + 1) * 12_000),
            text: (*text).to_string(),
            ..Default::default()
        })
        .collect();
    Lyrics {
        lines,
        timed: true,
        source: LyricsSource::None,
        ..Default::default()
    }
}

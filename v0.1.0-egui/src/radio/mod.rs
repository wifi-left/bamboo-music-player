//! Internet radio: a small user-managed station list plus sensible defaults.
//!
//! A radio track is just a [`Track`] with a [`TrackSource::Radio`] URL, so the
//! existing audio engine, HTTP source, queue and session persistence handle it
//! without a separate playback path.
//!
//! The custom-station list UI was replaced by the BambooMusic discovery page;
//! these helpers are retained for playlist parsing and future station support.

#![allow(dead_code)]

pub use crate::config::RadioStation;

use crate::model::{Track, TrackSource};

/// Defaults are public, long-running SomaFM MP3 streams. They are only a
/// starting point: users can delete them or add their own URLs.
pub fn default_stations() -> Vec<RadioStation> {
    const SOURCE: &str = "https://ice1.somafm.com";
    let entries = [
        (
            "Groove Salad",
            "groovesalad-128-mp3",
            "Chillout · Downtempo",
        ),
        ("Drone Zone", "dronezone-128-mp3", "Ambient · Drone"),
        (
            "Deep Space One",
            "deepspaceone-128-mp3",
            "Deep ambient · Space",
        ),
        ("Lush", "lush-128-mp3", "Vocal chill · Dream pop"),
        ("Secret Agent", "secretagent-128-mp3", "Spy jazz · Lounge"),
        ("Indie Pop Rocks", "indiepop-128-mp3", "Indie pop"),
        ("Folk Forward", "folkfwd-128-mp3", "Folk · Americana"),
        ("Fluid", "fluid-128-mp3", "Instrumental hip-hop"),
    ];
    entries
        .into_iter()
        .map(|(name, path, genre)| RadioStation {
            name: name.to_string(),
            url: format!("{SOURCE}/{path}"),
            genre: genre.to_string(),
        })
        .collect()
}

/// Normalize a user-entered stream address. Only absolute HTTP(S) URLs are
/// accepted; the audio engine handles redirects and ICY streams internally.
pub fn normalize_url(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed = url::Url::parse(trimmed).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    Some(parsed.to_string())
}

/// A friendly name for ad-hoc stream links: host plus the last path segment.
pub fn station_name_from_url(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else {
        return "网络电台".to_string();
    };
    let host = parsed.host_str().unwrap_or("radio").to_string();
    let last = parsed
        .path_segments()
        .and_then(|segments| segments.filter(|s| !s.is_empty()).next_back())
        .map(|s| s.trim_end_matches(|c: char| c.is_ascii_digit() || c == '-' || c == '.'))
        .filter(|s| !s.is_empty());
    match last {
        Some(path) if path.len() <= 40 => format!("{host} · {path}"),
        _ => host,
    }
}

pub fn track_for(station: &RadioStation) -> Track {
    let mut track = Track {
        source: TrackSource::Radio {
            url: station.url.clone(),
        },
        title: station.name.clone(),
        artist: station.genre.clone(),
        source_name: "网络电台".to_string(),
        ..Default::default()
    };
    track.apply_filename_fallback();
    track
}

pub fn station_from_input(name: &str, url: &str) -> Option<RadioStation> {
    let url = normalize_url(url)?;
    let name = name.trim();
    Some(RadioStation {
        name: if name.is_empty() {
            station_name_from_url(&url)
        } else {
            name.to_string()
        },
        url,
        genre: "自定义电台".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_http_streams_only() {
        assert_eq!(
            normalize_url(" https://ice1.somafm.com/groovesalad-128-mp3 "),
            Some("https://ice1.somafm.com/groovesalad-128-mp3".to_string())
        );
        assert!(normalize_url("file:///music/test.mp3").is_none());
        assert!(normalize_url("not a url").is_none());
    }

    #[test]
    fn station_track_round_trips_through_queue_key() {
        let station = RadioStation {
            name: "Test FM".to_string(),
            url: "https://example.com/live.mp3".to_string(),
            genre: "Test".to_string(),
        };
        let track = track_for(&station);
        assert_eq!(track.display_title(), "Test FM");
        assert!(
            track
                .dedupe_key()
                .starts_with("url:https://example.com/live.mp3")
        );
        assert!(matches!(track.source, TrackSource::Radio { .. }));
    }
}

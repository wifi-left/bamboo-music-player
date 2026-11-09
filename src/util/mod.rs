//! Small shared helpers.

pub mod cover;
pub mod icon;
pub mod image_util;
pub mod metadata;
pub mod share_image;

use std::path::Path;

pub fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "--:--".to_string();
    }
    let total = seconds.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn is_audio_file(path: &Path, extensions: &[String]) -> bool {
    path.extension()
        .map(|ext| {
            let ext = ext.to_string_lossy().to_ascii_lowercase();
            extensions.iter().any(|e| e == &ext)
        })
        .unwrap_or(false)
}

pub fn open_in_explorer(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// Reveal a file in Explorer and select it (`explorer /select,"path"`).
pub fn reveal_in_explorer(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        if let Some(parent) = path.parent() {
            let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
        }
    }
}

#[allow(dead_code)]
pub fn open_url(url: &str) {
    let _ = webbrowser::open(url);
}

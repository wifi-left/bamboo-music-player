//! System font loading (CJK) and egui font registration.
//!
//! The font file is memory-mapped instead of copied into the heap, which keeps
//! private memory lower for large CJK collections such as msyh.ttc.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use egui::{FontData, FontDefinitions, FontFamily};

use crate::config::Settings;

/// Named family used by the desktop lyrics window so it can use a different
/// font from the rest of the UI.
pub const DESKTOP_FONT_FAMILY: &str = "bamboo-desktop-lyrics";
/// Named family used by the immersive full-screen and standalone large lyrics
/// surfaces, so they can use a font independent from the rest of the UI.
pub const LARGE_FONT_FAMILY: &str = "bamboo-large-lyrics";

pub fn desktop_font_family() -> FontFamily {
    FontFamily::Name(DESKTOP_FONT_FAMILY.into())
}

pub fn large_lyrics_font_family() -> FontFamily {
    FontFamily::Name(LARGE_FONT_FAMILY.into())
}

pub fn install(ctx: &egui::Context, settings: &Settings) {
    let mut fonts = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);

    let family = settings
        .theme
        .font_family
        .as_deref()
        .unwrap_or("Microsoft YaHei UI");

    if let Some((bytes, index)) = load_system_font(family) {
        let data = FontData {
            font: bytes,
            index,
            tweak: Default::default(),
        };
        fonts.font_data.insert("bj-cjk".to_string(), Arc::new(data));
        if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
            list.insert(0, "bj-cjk".to_string());
        }
        if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
            list.push("bj-cjk".to_string());
        }
    } else {
        tracing::warn!("system font not found: {family}; CJK text may not render");
    }

    // The desktop lyrics font is independent from the UI font. Fall back to
    // the proportional family (including the CJK fallback) when unavailable.
    let mut desktop_list = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if let Some(desktop_family) = settings
        .desktop_lyrics
        .font_family
        .as_deref()
        .map(str::trim)
        .filter(|family| !family.is_empty())
    {
        if let Some((bytes, index)) = load_system_font(desktop_family) {
            let data = FontData {
                font: bytes,
                index,
                tweak: Default::default(),
            };
            fonts
                .font_data
                .insert("bamboo-desktop".to_string(), Arc::new(data));
            desktop_list.insert(0, "bamboo-desktop".to_string());
        } else {
            tracing::warn!("desktop lyrics font not found: {desktop_family}");
        }
    }
    fonts.families.insert(desktop_font_family(), desktop_list);

    // Large/full-screen lyrics get their own family as well. Falling back to
    // the proportional list keeps CJK support when the family is missing.
    let mut large_list = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if let Some(large_family) = settings
        .large_lyrics
        .font_family
        .as_deref()
        .map(str::trim)
        .filter(|family| !family.is_empty())
    {
        if let Some((bytes, index)) = load_system_font(large_family) {
            let data = FontData {
                font: bytes,
                index,
                tweak: Default::default(),
            };
            fonts
                .font_data
                .insert("bamboo-large".to_string(), Arc::new(data));
            large_list.insert(0, "bamboo-large".to_string());
        } else {
            tracing::warn!("large lyrics font not found: {large_family}");
        }
    }
    fonts.families.insert(large_lyrics_font_family(), large_list);

    ctx.set_fonts(fonts);
}

/// All font families installed on the system, sorted and de-duplicated.
/// Loading the database is expensive, so the result is cached for the
/// lifetime of the process.
pub fn system_font_families() -> &'static [String] {
    static FAMILIES: OnceLock<Vec<String>> = OnceLock::new();
    FAMILIES.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let mut names: Vec<String> = db
            .faces()
            .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        names.dedup();
        names
    })
}

fn load_system_font(family: &str) -> Option<(Cow<'static, [u8]>, u32)> {
    // Allow the setting to point directly at a font file.
    let as_path = Path::new(family);
    if as_path.is_file() {
        return mmap_font(as_path).map(|bytes| (bytes, 0));
    }

    if let Some((file, index)) = known_font_file(family) {
        if let Some(bytes) = mmap_font(&file) {
            return Some((bytes, index));
        }
    }

    // Fallback: let fontdb resolve the family, then map the chosen file.
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let families = [
        fontdb::Family::Name(family),
        fontdb::Family::Name("Microsoft YaHei UI"),
        fontdb::Family::Name("Microsoft YaHei"),
        fontdb::Family::Name("Segoe UI"),
    ];
    let query = fontdb::Query {
        families: &families,
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    };
    let id = db.query(&query)?;
    let face = db.face(id)?;
    let index = face.index;
    match &face.source {
        fontdb::Source::File(path) => mmap_font(path).map(|bytes| (bytes, index)),
        fontdb::Source::Binary(data) => Some((Cow::Owned(data.as_ref().as_ref().to_vec()), index)),
        _ => None,
    }
}

fn known_font_file(family: &str) -> Option<(PathBuf, u32)> {
    let lower = family.to_ascii_lowercase();
    let entry: (&str, u32) = if lower.contains("yahei") {
        ("msyh.ttc", 0)
    } else if lower.contains("simhei") {
        ("simhei.ttf", 0)
    } else if lower.contains("deng") || lower.contains("等线") {
        ("Deng.ttf", 0)
    } else if lower.contains("simsun") {
        ("simsun.ttc", 0)
    } else if lower.contains("segoe") {
        ("segoeui.ttf", 0)
    } else if lower.contains("calibri") {
        ("calibri.ttf", 0)
    } else if lower.contains("arial") {
        ("arial.ttf", 0)
    } else {
        return None;
    };
    let windir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    let path = windir.join("Fonts").join(entry.0);
    path.is_file().then_some((path, entry.1))
}

fn mmap_font(path: &Path) -> Option<Cow<'static, [u8]>> {
    let file = std::fs::File::open(path).ok()?;
    let mmap = unsafe { memmap2::Mmap::map(&file).ok()? };
    let leaked: &'static memmap2::Mmap = Box::leak(Box::new(mmap));
    Some(Cow::Borrowed(&**leaked))
}

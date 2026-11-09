//! Shared desktop-lyrics helpers.
//!
//! The renderer itself lives in a native Win32 + Direct2D window thread inside
//! the player. The player only keeps the labels and the on-screen clamping used
//! by the settings page and mini-player placement.

use egui::{CursorIcon, Pos2, Rect, ResizeDirection};

use crate::config::LyricsAlign;

/// Align helper kept here so the settings UI and the native renderer agree.
pub fn align_label(align: LyricsAlign) -> &'static str {
    match align {
        LyricsAlign::Left => crate::i18n::tr("左对齐"),
        LyricsAlign::Center => crate::i18n::tr("居中"),
        LyricsAlign::Right => crate::i18n::tr("右对齐"),
    }
}

/// Narrow edge band used by the egui mini/standalone windows.
pub(crate) fn edge_resize_direction(rect: Rect, pointer: Pos2) -> Option<ResizeDirection> {
    const EDGE: f32 = 4.0;
    if !rect.expand(EDGE).contains(pointer) {
        return None;
    }
    let left = pointer.x <= rect.left() + EDGE;
    let right = pointer.x >= rect.right() - EDGE;
    let top = pointer.y <= rect.top() + EDGE;
    let bottom = pointer.y >= rect.bottom() - EDGE;
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (_, true, true, _) => Some(ResizeDirection::NorthEast),
        (true, _, _, true) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (true, _, _, _) => Some(ResizeDirection::West),
        (_, true, _, _) => Some(ResizeDirection::East),
        (_, _, true, _) => Some(ResizeDirection::North),
        (_, _, _, true) => Some(ResizeDirection::South),
        _ => None,
    }
}

pub(crate) fn resize_cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::ResizeVertical,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::ResizeHorizontal,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::ResizeNeSw,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::ResizeNwSe,
    }
}

/// Keep a restored position on-screen when it would otherwise land outside
/// the primary monitor. Positions on secondary monitors are preserved.
pub fn clamp_position(pos: Pos2, width: f32, height: f32) -> Pos2 {
    if let Some((wx, wy, ww, wh)) = crate::platform::window::work_area_logical() {
        let inside_primary = pos.x >= wx - 40.0
            && pos.x <= wx + ww + 40.0
            && pos.y >= wy - 40.0
            && pos.y <= wy + wh + 40.0;
        if inside_primary {
            let x = pos.x.clamp(wx, (wx + ww - width).max(wx));
            let y = pos.y.clamp(wy, (wy + wh - height).max(wy));
            return Pos2::new(x, y);
        }
    }
    pos
}

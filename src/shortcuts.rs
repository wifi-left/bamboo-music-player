//! Global hotkey parsing and formatting.
//!
//! Settings store combinations as human-readable text (`Ctrl+Alt+Space`).
//! The shell parses them into Win32 modifier/virtual-key pairs for
//! `RegisterHotKey`, and key capture builds the same text back. Parsing is
//! shared by the conflict check so `ctrl+alt+space` and `Ctrl+Alt+Space`
//! cannot both be bound.

pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;
/// Win32 modifier that suppresses auto-repeat while the key is held.
pub const MOD_NOREPEAT: u32 = 0x4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u32,
    pub vk: u32,
}

/// True for a key that only changes modifier state; key capture waits until a
/// real key arrives.
pub fn is_modifier_key(vk: u32) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C)
}

/// Parse from the order-independent `Modifier+...+Key` form. At least one
/// modifier is required: a bare global key would swallow normal typing.
pub fn parse(text: &str) -> Option<Hotkey> {
    let mut modifiers = 0u32;
    let mut key = None;
    for token in text.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        let lower = token.to_ascii_lowercase();
        let modifier = match lower.as_str() {
            "ctrl" | "control" => Some(MOD_CONTROL),
            "alt" | "menu" => Some(MOD_ALT),
            "shift" => Some(MOD_SHIFT),
            "win" | "windows" | "super" => Some(MOD_WIN),
            _ => None,
        };
        match modifier {
            Some(flag) => modifiers |= flag,
            None => {
                if key.is_some() {
                    return None;
                }
                key = key_from_name(token);
            }
        }
    }
    let vk = key?;
    if modifiers == 0 || is_modifier_key(vk) {
        return None;
    }
    Some(Hotkey { modifiers, vk })
}

/// Build the canonical text for a Win32 combination. Returns `None` for a
/// modifier-only or unknown key.
pub fn format(modifiers: u32, vk: u32) -> Option<String> {
    if modifiers == 0 || is_modifier_key(vk) {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for (flag, name) in [
        (MOD_CONTROL, "Ctrl"),
        (MOD_ALT, "Alt"),
        (MOD_SHIFT, "Shift"),
        (MOD_WIN, "Win"),
    ] {
        if modifiers & flag != 0 {
            parts.push(name.to_string());
        }
    }
    parts.push(name_from_key(vk)?);
    Some(parts.join("+"))
}

/// Build the display text from the modifier state of the pressed keys.
pub fn from_key(vk: u16, ctrl: bool, alt: bool, shift: bool, win: bool) -> Option<String> {
    let mut modifiers = 0;
    if ctrl {
        modifiers |= MOD_CONTROL;
    }
    if alt {
        modifiers |= MOD_ALT;
    }
    if shift {
        modifiers |= MOD_SHIFT;
    }
    if win {
        modifiers |= MOD_WIN;
    }
    format(modifiers, vk as u32)
}

fn key_from_name(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    if upper.len() == 1 {
        let ch = upper.chars().next()?;
        if ch.is_ascii_alphanumeric() {
            return Some(ch as u32);
        }
        return None;
    }
    let named = match upper.as_str() {
        "SPACE" => 0x20,
        "ENTER" | "RETURN" => 0x0D,
        "TAB" => 0x09,
        "ESC" | "ESCAPE" => 0x1B,
        "BACKSPACE" => 0x08,
        "INSERT" | "INS" => 0x2D,
        "DELETE" | "DEL" => 0x2E,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PRIOR" => 0x21,
        "PAGEDOWN" | "NEXT" => 0x22,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "PRINTSCREEN" | "SNAPSHOT" => 0x2C,
        "PAUSE" => 0x13,
        "CAPSLOCK" | "CAPITAL" => 0x14,
        "NUMLOCK" => 0x90,
        "SCROLLLOCK" => 0x91,
        "MEDIANEXT" => 0xB0,
        "MEDIAPREV" => 0xB1,
        "MEDIASTOP" => 0xB2,
        "MEDIAPLAYPAUSE" | "MEDIAPLAY" => 0xB3,
        "VOLUMEMUTE" => 0xAD,
        "VOLUMEDOWN" => 0xAE,
        "VOLUMEUP" => 0xAF,
        _ => {
            let number = upper.strip_prefix('F')?.parse::<u32>().ok()?;
            if (1..=24).contains(&number) {
                0x70 + number - 1
            } else {
                return None;
            }
        }
    };
    Some(named)
}

fn name_from_key(vk: u32) -> Option<String> {
    if (0x41..=0x5A).contains(&vk) || (0x30..=0x39).contains(&vk) {
        return Some(char::from_u32(vk)?.to_string());
    }
    if (0x70..=0x87).contains(&vk) {
        return Some(format!("F{}", vk - 0x70 + 1));
    }
    let name = match vk {
        0x20 => "Space",
        0x0D => "Enter",
        0x09 => "Tab",
        0x1B => "Escape",
        0x08 => "Backspace",
        0x2D => "Insert",
        0x2E => "Delete",
        0x24 => "Home",
        0x23 => "End",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0x2C => "PrintScreen",
        0x13 => "Pause",
        0x14 => "CapsLock",
        0x90 => "NumLock",
        0x91 => "ScrollLock",
        0xB0 => "MediaNext",
        0xB1 => "MediaPrev",
        0xB2 => "MediaStop",
        0xB3 => "MediaPlayPause",
        0xAD => "VolumeMute",
        0xAE => "VolumeDown",
        0xAF => "VolumeUp",
        _ => return None,
    };
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_combinations_parse() {
        let hotkey = parse("Ctrl+Alt+Space").unwrap();
        assert_eq!(hotkey.vk, 0x20);
        assert_eq!(hotkey.modifiers, MOD_CONTROL | MOD_ALT);
        assert!(parse("Ctrl+Alt+Right").is_some());
        assert!(parse("Ctrl+Alt+L").is_some());
    }

    #[test]
    fn default_global_control_bindings() {
        let settings = crate::config::ShortcutSettings::default();
        assert_eq!(settings.play_pause, "Ctrl+Alt+P");
        assert_eq!(settings.prev, "Ctrl+Alt+Left");
        assert_eq!(settings.next, "Ctrl+Alt+Right");
        assert_eq!(settings.toggle_desktop_lyrics, "Ctrl+Alt+M");
        assert_eq!(settings.show_hide, "Ctrl+Alt+Q");
        assert_eq!(settings.copy_lyric, "Ctrl+Alt+L");
        assert_eq!(settings.copy_title, "Ctrl+Alt+T");
        assert_eq!(settings.fill_lyric, "Ctrl+Alt+Shift+L");
        assert_eq!(settings.fill_title, "Ctrl+Alt+Shift+T");
        assert!(settings.gamepad);
    }

    #[test]
    fn parsing_is_order_and_case_insensitive() {
        let a = parse("ctrl+alt+space").unwrap();
        let b = parse("Alt + Ctrl + Space").unwrap();
        assert_eq!(a, b);
        assert_eq!(format(a.modifiers, a.vk).unwrap(), "Ctrl+Alt+Space");
    }

    #[test]
    fn bare_keys_and_modifier_only_are_rejected() {
        assert!(parse("Space").is_none());
        assert!(parse("Ctrl").is_none());
        assert!(parse("Ctrl+Shift").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn capture_requires_a_modifier() {
        assert_eq!(
            from_key(0x20, true, false, false, false).unwrap(),
            "Ctrl+Space"
        );
        assert!(from_key(0x20, false, false, false, false).is_none());
        assert!(from_key(0x11, true, false, false, false).is_none());
    }

    #[test]
    fn no_repeat_flag_does_not_change_identity() {
        // Registration adds MOD_NOREPEAT; parsing must not treat it as part of
        // the binding, otherwise duplicates could slip through.
        let parsed = parse("Ctrl+Alt+L").unwrap();
        assert_eq!(parsed.modifiers & MOD_NOREPEAT, 0);
    }
}

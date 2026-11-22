//! XInput gamepad support.
//!
//! The DLL is loaded lazily; when no controller is connected the shell starts
//! no polling timer at all and only re-checks on `WM_DEVICECHANGE`. While a
//! controller is connected, a 100 ms timer samples button edges.
//!
//! Settings store shortcuts as button chords (`LB+RB`, `LB+Start`). Chords are
//! deliberate: a game that uses single face buttons cannot trigger the player
//! by accident, and holding both bumpers plus Start is not something a normal
//! gameplay session produces. The left stick and the D-pad additionally
//! auto-repeat so the same poll drives focus-ring navigation.
//!
//! See [`Poll`] for the raw edge state the shell consumes.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::s;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct XInputGamepad {
    buttons: u16,
    left_trigger: u8,
    right_trigger: u8,
    thumb_lx: i16,
    thumb_ly: i16,
    thumb_rx: i16,
    thumb_ry: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct XInputState {
    packet: u32,
    gamepad: XInputGamepad,
}

type GetState = unsafe extern "system" fn(u32, *mut XInputState) -> u32;

const ERROR_SUCCESS: u32 = 0;
const ERROR_DEVICE_NOT_CONNECTED: u32 = 1167;

pub const DPAD_UP: u32 = 0x0001;
pub const DPAD_DOWN: u32 = 0x0002;
pub const DPAD_LEFT: u32 = 0x0004;
pub const DPAD_RIGHT: u32 = 0x0008;
pub const START: u32 = 0x0010;
pub const BACK: u32 = 0x0020;
pub const LEFT_THUMB: u32 = 0x0040;
pub const RIGHT_THUMB: u32 = 0x0080;
pub const LB: u32 = 0x0100;
pub const RB: u32 = 0x0200;
pub const A: u32 = 0x1000;
pub const B: u32 = 0x2000;
pub const X: u32 = 0x4000;
pub const Y: u32 = 0x8000;

// XInput reports the triggers as analog bytes, not as button bits. They are
// promoted to pseudo-buttons so they can take part in chords (`LT+RT`, ...).
pub const LT: u32 = 0x0001_0000;
pub const RT: u32 = 0x0002_0000;

/// Buttons the focus-ring navigation consumes while the player is focused.
/// Global combos containing any of these are shadowed by the UI, mirroring how
/// an in-window keyboard shortcut takes precedence over a global hotkey.
pub const UI_MASK: u32 =
    A | B | START | LB | RB | DPAD_UP | DPAD_DOWN | DPAD_LEFT | DPAD_RIGHT;

/// Trigger travel that counts as pressed; releasing needs a little hysteresis
/// so an analog trigger resting near the threshold cannot chatter.
const TRIGGER_DOWN: u8 = 100;
const TRIGGER_UP: u8 = 60;

/// Left-stick magnitude (fraction of full deflection) before a direction is
/// emitted; 0.42 keeps drift and light resting contact quiet.
const STICK_DEADZONE: f32 = 0.42;
/// First repeat after a direction is held, then one per poll tick.
const REPEAT_DELAY: Duration = Duration::from_millis(380);
const REPEAT_PERIOD: Duration = Duration::from_millis(110);

/// Canonical order and names. Parsing is order- and case-insensitive; formatting
/// always emits this order so equal chords share one settings string.
const BUTTONS: [(u32, &str); 16] = [
    (LB, "LB"),
    (RB, "RB"),
    (LT, "LT"),
    (RT, "RT"),
    (BACK, "Back"),
    (START, "Start"),
    (LEFT_THUMB, "LS"),
    (RIGHT_THUMB, "RS"),
    (DPAD_UP, "Up"),
    (DPAD_DOWN, "Down"),
    (DPAD_LEFT, "Left"),
    (DPAD_RIGHT, "Right"),
    (A, "A"),
    (B, "B"),
    (X, "X"),
    (Y, "Y"),
];

/// Parse `A+B+...` into a button mask. Unknown names reject the whole chord,
/// and an empty string is "unbound" rather than a mask of zero.
pub fn parse(text: &str) -> Option<u32> {
    let mut mask = 0u32;
    let mut any = false;
    for token in text.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        mask |= button_from_name(token)?;
        any = true;
    }
    any.then_some(mask)
}

/// Canonical display text. `None` for an empty/invalid mask.
pub fn format(mask: u32) -> Option<String> {
    if mask == 0 {
        return None;
    }
    let mut parts = Vec::new();
    for (bit, name) in BUTTONS {
        if mask & bit != 0 {
            parts.push(name);
        }
    }
    // Bits outside the table would silently vanish; refuse them instead of
    // handing the user a different binding than the one that was captured.
    if parts.is_empty() || mask & !ALL_BUTTONS != 0 {
        return None;
    }
    Some(parts.join("+"))
}

/// True when the chord needs more than one physical button. Single-button
/// bindings are the ones that leak into fullscreen games.
pub fn is_chord(mask: u32) -> bool {
    mask.count_ones() >= 2
}

pub const ALL_BUTTONS: u32 = LB
    | RB
    | LT
    | RT
    | BACK
    | START
    | LEFT_THUMB
    | RIGHT_THUMB
    | DPAD_UP
    | DPAD_DOWN
    | DPAD_LEFT
    | DPAD_RIGHT
    | A
    | B
    | X
    | Y;

fn button_from_name(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase().replace([' ', '_', '-'], "");
    let bit = match lower.as_str() {
        "a" => A,
        "b" => B,
        "x" => X,
        "y" => Y,
        "lb" | "l1" => LB,
        "rb" | "r1" => RB,
        "lt" | "l2" | "zl" => LT,
        "rt" | "r2" | "zr" => RT,
        "start" | "plus" => START,
        "back" | "select" | "minus" | "share" => BACK,
        "ls" | "l3" | "leftstick" => LEFT_THUMB,
        "rs" | "r3" | "rightstick" => RIGHT_THUMB,
        "up" | "dpadup" => DPAD_UP,
        "down" | "dpaddown" => DPAD_DOWN,
        "left" | "dpadleft" => DPAD_LEFT,
        "right" | "dpadright" => DPAD_RIGHT,
        _ => return None,
    };
    Some(bit)
}

/// Direction of the D-pad / left stick, already filtered through the auto-repeat
/// state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadDir {
    Up,
    Down,
    Left,
    Right,
}

impl PadDir {
    /// Virtual key the focus-ring navigation understands.
    pub fn key(self) -> u16 {
        match self {
            PadDir::Left => 0x25,
            PadDir::Up => 0x26,
            PadDir::Right => 0x27,
            PadDir::Down => 0x28,
        }
    }
}

/// One poll's worth of input state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Poll {
    /// Buttons held at the previous poll, used to find completed chords.
    pub previous: u32,
    /// Every button held right now (including the trigger pseudo-buttons).
    pub down: u32,
    /// Buttons that went down since the previous poll.
    pub pressed: u32,
    /// Every button was released since the previous poll. Used by capture mode
    /// to know that a chord is complete.
    pub released: bool,
    /// One navigation step: a fresh D-pad/stick deflection or auto-repeat.
    pub direction: Option<PadDir>,
}

/// Chords that completed on this poll. A chord fires when all of its buttons
/// are down and at least one of them was not down before. When chords overlap,
/// the longest one wins, so a single-button binding never fires alongside a
/// chord that contains it.
pub fn triggered(prev: u32, down: u32, combos: &[u32]) -> Vec<usize> {
    let complete: Vec<(usize, u32)> = combos
        .iter()
        .enumerate()
        .filter(|(_, combo)| **combo != 0 && down & **combo == **combo && prev & **combo != **combo)
        .map(|(index, combo)| (index, *combo))
        .collect();
    let masks: Vec<u32> = complete.iter().map(|(_, mask)| *mask).collect();
    complete
        .into_iter()
        .filter(|(_, mask)| {
            !masks
                .iter()
                .any(|other| *other != *mask && (*other & *mask) == *mask && other.count_ones() > mask.count_ones())
        })
        .map(|(index, _)| index)
        .collect()
}

pub struct Gamepad {
    module: Option<HMODULE>,
    get_state: Option<GetState>,
    connected: bool,
    buttons: u32,
    left_trigger: bool,
    right_trigger: bool,
    direction: Option<PadDir>,
    next_repeat: Option<Instant>,
}

impl Gamepad {
    pub fn new() -> Self {
        let mut module = None;
        for name in ["xinput1_4.dll", "xinput9_1_0.dll"] {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            if let Ok(handle) = unsafe { LoadLibraryW(windows::core::PCWSTR(wide.as_ptr())) } {
                module = Some(handle);
                break;
            }
        }
        let get_state = module.and_then(|module| {
            let address = unsafe { GetProcAddress(module, s!("XInputGetState")) }?;
            // The signature is fixed by XInput's public ABI.
            Some(unsafe {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, GetState>(address)
            })
        });
        let mut gamepad = Self {
            module,
            get_state,
            connected: false,
            buttons: 0,
            left_trigger: false,
            right_trigger: false,
            direction: None,
            next_repeat: None,
        };
        gamepad.connected = gamepad.sample().is_some();
        gamepad
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Ask XInput directly. Used by `WM_DEVICECHANGE` between poll ticks.
    pub fn refresh_connection(&mut self) -> bool {
        let state = self.sample();
        self.connected = state.is_some();
        if let Some(sample) = state {
            self.buttons = self.mask(&sample);
        }
        self.connected
    }

    fn sample(&self) -> Option<Sample> {
        let get_state = self.get_state?;
        let mut state = XInputState::default();
        let result = unsafe { get_state(0, &mut state) };
        match result {
            ERROR_SUCCESS => Some(Sample {
                buttons: state.gamepad.buttons,
                left_trigger: state.gamepad.left_trigger,
                right_trigger: state.gamepad.right_trigger,
                thumb_lx: state.gamepad.thumb_lx,
                thumb_ly: state.gamepad.thumb_ly,
            }),
            ERROR_DEVICE_NOT_CONNECTED => None,
            _ => None,
        }
    }

    fn mask(&mut self, sample: &Sample) -> u32 {
        self.left_trigger = if sample.left_trigger >= TRIGGER_DOWN {
            true
        } else if sample.left_trigger <= TRIGGER_UP {
            false
        } else {
            self.left_trigger
        };
        self.right_trigger = if sample.right_trigger >= TRIGGER_DOWN {
            true
        } else if sample.right_trigger <= TRIGGER_UP {
            false
        } else {
            self.right_trigger
        };
        let mut mask = sample.buttons as u32;
        if self.left_trigger {
            mask |= LT;
        }
        if self.right_trigger {
            mask |= RT;
        }
        mask
    }

    /// Sample and advance the edge/repeat state. `None` means the controller
    /// went away; the caller stops the timer.
    pub fn poll(&mut self) -> Option<Poll> {
        let Some(sample) = self.sample() else {
            self.connected = false;
            self.buttons = 0;
            self.direction = None;
            self.next_repeat = None;
            return None;
        };
        self.connected = true;
        let down = self.mask(&sample);
        let previous = self.buttons;
        let pressed = down & !previous;
        let released = previous != 0 && down == 0;
        self.buttons = down;
        let raw = dpad_direction(down).or_else(|| stick_direction(&sample));
        let direction = self.repeat(raw);
        Some(Poll {
            previous,
            down,
            pressed,
            released,
            direction,
        })
    }

    /// Emit a direction on change, then repeat while it is held.
    fn repeat(&mut self, raw: Option<PadDir>) -> Option<PadDir> {
        let now = Instant::now();
        if raw != self.direction {
            self.direction = raw;
            self.next_repeat = raw.map(|_| now + REPEAT_DELAY);
            return raw;
        }
        let held = raw?;
        match self.next_repeat {
            Some(at) if now >= at => {
                self.next_repeat = Some(now + REPEAT_PERIOD);
                Some(held)
            }
            _ => None,
        }
    }
}

struct Sample {
    buttons: u16,
    left_trigger: u8,
    right_trigger: u8,
    thumb_lx: i16,
    thumb_ly: i16,
}

/// D-pad wins over the stick: it is digital and a player pressing both means
/// the D-pad. Multiple directions resolve in a fixed order so the repeat state
/// does not flip while the stick rolls around a diagonal.
fn dpad_direction(down: u32) -> Option<PadDir> {
    if down & DPAD_UP != 0 {
        Some(PadDir::Up)
    } else if down & DPAD_DOWN != 0 {
        Some(PadDir::Down)
    } else if down & DPAD_LEFT != 0 {
        Some(PadDir::Left)
    } else if down & DPAD_RIGHT != 0 {
        Some(PadDir::Right)
    } else {
        None
    }
}

fn stick_direction(sample: &Sample) -> Option<PadDir> {
    let x = sample.thumb_lx as f32 / i16::MAX as f32;
    let y = sample.thumb_ly as f32 / i16::MAX as f32;
    if x.abs().max(y.abs()) < STICK_DEADZONE {
        return None;
    }
    if x.abs() > y.abs() {
        Some(if x > 0.0 { PadDir::Right } else { PadDir::Left })
    } else {
        // XInput's left-stick Y axis is positive when pushed up.
        Some(if y > 0.0 { PadDir::Up } else { PadDir::Down })
    }
}

impl Default for Gamepad {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Gamepad {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            unsafe {
                let _ = FreeLibrary(module);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_masks_are_distinct() {
        let masks: Vec<u32> = BUTTONS.iter().map(|(mask, _)| *mask).collect();
        for (index, mask) in masks.iter().enumerate() {
            assert_eq!(mask.count_ones(), 1);
            assert!(!masks[index + 1..].contains(mask));
        }
    }

    #[test]
    fn xinput_state_layout_matches_the_public_abi() {
        assert_eq!(std::mem::size_of::<XInputGamepad>(), 12);
        assert_eq!(std::mem::size_of::<XInputState>(), 16);
    }

    #[test]
    fn missing_dll_is_not_a_connection() {
        // A machine without XInput simply reports "not connected" instead of
        // panicking; the shell then never starts the poll timer.
        let mut gamepad = Gamepad {
            module: None,
            get_state: None,
            connected: false,
            buttons: 0,
            left_trigger: false,
            right_trigger: false,
            direction: None,
            next_repeat: None,
        };
        assert!(!gamepad.is_connected());
        assert!(gamepad.sample().is_none());
        assert!(gamepad.poll().is_none());
    }

    #[test]
    fn chords_parse_order_insensitively_and_format_canonically() {
        let a = parse("lb+rb").unwrap();
        let b = parse("Rb + Lb").unwrap();
        assert_eq!(a, b);
        assert_eq!(format(a).unwrap(), "LB+RB");
        assert_eq!(parse("Start+A").unwrap(), START | A);
        assert_eq!(format(A | START).unwrap(), "Start+A");
    }

    #[test]
    fn aliases_share_one_canonical_string() {
        assert_eq!(parse("l1").unwrap(), LB);
        assert_eq!(parse("LeftStick").unwrap(), LEFT_THUMB);
        assert_eq!(parse("dpad-up").unwrap(), DPAD_UP);
        assert_eq!(format(parse("r2").unwrap()).unwrap(), "RT");
    }

    #[test]
    fn empty_and_unknown_chords_are_rejected() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("   "), None);
        assert_eq!(parse("A+Bogus"), None);
        assert_eq!(format(0), None);
    }

    #[test]
    fn single_button_is_not_a_chord() {
        assert!(!is_chord(A));
        assert!(is_chord(LB | A));
        assert!(is_chord(START | BACK));
    }

    #[test]
    fn a_chord_suppresses_its_single_button_subset() {
        let combos = [A, LB | A, LB];
        // A and LB pressed together: the chord wins, singles stay silent.
        assert_eq!(triggered(0, A | LB, &combos), vec![1]);
        // A alone: only the single binding, and only on the press edge.
        assert_eq!(triggered(0, A, &combos), vec![0]);
        assert!(triggered(A, A, &combos).is_empty());
        // Adding LB while A is already held completes the chord.
        assert_eq!(triggered(A, A | LB, &combos), vec![1]);
    }

    #[test]
    fn direction_keys_match_the_focus_ring() {
        assert_eq!(PadDir::Up.key(), 0x26);
        assert_eq!(PadDir::Down.key(), 0x28);
        assert_eq!(PadDir::Left.key(), 0x25);
        assert_eq!(PadDir::Right.key(), 0x27);
    }

    #[test]
    fn stick_axis_direction_matches_xinput() {
        // XInput reports a pushed-up stick as a positive sThumbLY; getting
        // this backwards made the focus ring move opposite to the stick.
        let sample = |lx: i16, ly: i16| Sample {
            buttons: 0,
            left_trigger: 0,
            right_trigger: 0,
            thumb_lx: lx,
            thumb_ly: ly,
        };
        assert_eq!(stick_direction(&sample(0, i16::MAX)), Some(PadDir::Up));
        assert_eq!(stick_direction(&sample(0, i16::MIN)), Some(PadDir::Down));
        assert_eq!(stick_direction(&sample(i16::MAX, 0)), Some(PadDir::Right));
        assert_eq!(stick_direction(&sample(i16::MIN, 0)), Some(PadDir::Left));
        assert_eq!(stick_direction(&sample(0, 0)), None);
    }
}

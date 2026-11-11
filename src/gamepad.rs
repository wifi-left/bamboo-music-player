//! XInput gamepad support.
//!
//! The DLL is loaded lazily; when no controller is connected the shell starts
//! no polling timer at all and only re-checks on `WM_DEVICECHANGE`. While a
//! controller is connected, a 100 ms timer samples button edges and maps them
//! to playback commands. No UI repaint is involved.

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

const DPAD_UP: u16 = 0x0001;
const DPAD_DOWN: u16 = 0x0002;
const DPAD_LEFT: u16 = 0x0004;
const DPAD_RIGHT: u16 = 0x0008;
const START: u16 = 0x0010;
const A: u16 = 0x1000;
const B: u16 = 0x2000;
const X: u16 = 0x4000;
const Y: u16 = 0x8000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamepadAction {
    PlayPause,
    Next,
    Prev,
    VolumeUp,
    VolumeDown,
    SeekBack,
    SeekForward,
    ToggleDesktopLyrics,
    ShowHide,
}

pub struct Gamepad {
    module: Option<HMODULE>,
    get_state: Option<GetState>,
    connected: bool,
    buttons: u16,
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
        if let Some((_, buttons)) = state {
            self.buttons = buttons;
        }
        self.connected
    }

    fn sample(&self) -> Option<(u32, u16)> {
        let get_state = self.get_state?;
        let mut state = XInputState::default();
        let result = unsafe { get_state(0, &mut state) };
        match result {
            ERROR_SUCCESS => Some((state.packet, state.gamepad.buttons)),
            ERROR_DEVICE_NOT_CONNECTED => None,
            _ => None,
        }
    }

    /// Newly pressed buttons since the previous poll.
    pub fn poll(&mut self) -> Vec<GamepadAction> {
        let Some((_, buttons)) = self.sample() else {
            self.connected = false;
            self.buttons = 0;
            return Vec::new();
        };
        self.connected = true;
        let pressed = buttons & !self.buttons;
        self.buttons = buttons;
        let mut actions = Vec::new();
        for (mask, action) in [
            (A, GamepadAction::PlayPause),
            (B, GamepadAction::Next),
            (X, GamepadAction::Prev),
            (Y, GamepadAction::ToggleDesktopLyrics),
            (START, GamepadAction::ShowHide),
            (DPAD_UP, GamepadAction::VolumeUp),
            (DPAD_DOWN, GamepadAction::VolumeDown),
            (DPAD_LEFT, GamepadAction::SeekBack),
            (DPAD_RIGHT, GamepadAction::SeekForward),
        ] {
            if pressed & mask != 0 {
                actions.push(action);
            }
        }
        actions
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
        let masks = [DPAD_UP, DPAD_DOWN, DPAD_LEFT, DPAD_RIGHT, START, A, B, X, Y];
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
        let gamepad = Gamepad {
            module: None,
            get_state: None,
            connected: false,
            buttons: 0,
        };
        assert!(!gamepad.is_connected());
        assert!(gamepad.sample().is_none());
    }
}

//! Inject text into the window under the mouse cursor.
//!
//! The fill hotkeys use the same path an IME uses to commit text: the target
//! window is brought to the foreground and UTF-16 code units are synthesised
//! with `SendInput` + `KEYEVENTF_UNICODE`. That reaches ordinary Win32
//! controls, Chromium/Electron text fields and many custom editors without
//! depending on the clipboard or keyboard layout.
//!
//! When Windows refuses the foreground switch, the characters are posted as
//! `WM_CHAR` to the target thread's focused control as a best-effort fallback
//! (classic controls accept it; some sandboxed editors ignore it).

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GUITHREADINFO, GetAncestor, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo,
    GetWindowThreadProcessId, IsWindowVisible, PostMessageW, SetForegroundWindow, WM_CHAR,
    WindowFromPoint,
};

/// Type `text` into the focused control of the top-level window under the
/// cursor. `own_window` is the player's main window: other windows of this
/// process (desktop lyrics, mini player) are skipped because they cannot take
/// keyboard focus. Returns false when no text could be delivered.
pub fn type_under_cursor(text: &str, own_window: HWND) -> bool {
    if text.is_empty() {
        return false;
    }
    let Some(root) = window_under_cursor() else {
        return false;
    };
    if root != own_window && process_id(root) == unsafe { GetCurrentProcessId() } {
        return false;
    }
    if !unsafe { IsWindowVisible(root) }.as_bool() {
        return false;
    }
    if unsafe { GetForegroundWindow() } != root {
        unsafe {
            let _ = SetForegroundWindow(root);
        }
        let deadline = Instant::now() + Duration::from_millis(180);
        while Instant::now() < deadline && unsafe { GetForegroundWindow() } != root {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if unsafe { GetForegroundWindow() } == root {
        send_unicode(text)
    } else {
        post_chars(focused_control(root), text)
    }
}

fn window_under_cursor() -> Option<HWND> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }.ok()?;
    let hit = unsafe { WindowFromPoint(point) };
    if hit.is_invalid() {
        return None;
    }
    let root = unsafe { GetAncestor(hit, GA_ROOT) };
    (!root.is_invalid()).then_some(root)
}

fn process_id(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
}

fn focused_control(root: HWND) -> HWND {
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let thread = unsafe { GetWindowThreadProcessId(root, None) };
    if unsafe { GetGUIThreadInfo(thread, &mut info) }.is_ok() && !info.hwndFocus.is_invalid() {
        info.hwndFocus
    } else {
        root
    }
}

fn send_unicode(text: &str) -> bool {
    for chunk in text.encode_utf16().collect::<Vec<_>>().chunks(64) {
        let mut inputs = Vec::with_capacity(chunk.len() * 2);
        for unit in chunk {
            inputs.push(key_event(*unit, KEYEVENTF_UNICODE));
            inputs.push(key_event(*unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
        }
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            return false;
        }
        // A tiny gap lets slower message loops keep character order and keeps
        // the UI thread of the target responsive for long titles.
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

fn key_event(scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn post_chars(target: HWND, text: &str) -> bool {
    if target.is_invalid() {
        return false;
    }
    for unit in text.encode_utf16() {
        let posted = unsafe {
            PostMessageW(
                Some(target),
                WM_CHAR,
                WPARAM(unit as usize),
                LPARAM(0),
            )
        };
        if posted.is_err() {
            return false;
        }
    }
    true
}

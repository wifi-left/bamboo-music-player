//! Event-driven wake for the layered overlay windows.
//!
//! The overlay threads own a Win32 message loop; they used to poll their
//! crossbeam inbox from a 16 ms render timer. That kept a timer alive while the
//! overlay was paused and did work on every tick even when nothing on screen
//! changed. Instead a tiny forwarder thread blocks on the inbox, enqueues the
//! message and posts `WM_APP_OVERLAY_WAKE` to the overlay window. The window
//! can then keep its render timer stopped whenever it is idle.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crossbeam_channel::Receiver;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Posted by the forwarder thread after a host message was queued.
pub const WM_APP_OVERLAY_WAKE: u32 = 0x8000 + 81;

struct Shared<T> {
    queue: Mutex<VecDeque<T>>,
    /// Stops the forwarder when the window is gone; otherwise it would keep
    /// waking a dead `HWND`.
    closed: Mutex<bool>,
}

/// A channel-to-window-message bridge. Messages must be drained from the
/// window procedure in response to [`WM_APP_OVERLAY_WAKE`].
pub struct Inbox<T> {
    shared: Arc<Shared<T>>,
}

impl<T: Send + 'static> Inbox<T> {
    /// Start forwarding `receiver` to `hwnd`. The thread exits when the
    /// channel closes, the window disappears, or [`Inbox::close`] is called.
    pub fn start(receiver: Receiver<T>, hwnd: HWND) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            closed: Mutex::new(false),
        });
        let sink = shared.clone();
        // `HWND` is a raw pointer wrapper and not `Send`; the handle value is
        // stable for the lifetime of the overlay thread.
        let raw = hwnd.0 as isize;
        let _ = std::thread::Builder::new()
            .name("bamboo-overlay-waker".to_string())
            .spawn(move || {
                while let Ok(message) = receiver.recv() {
                    if *sink.closed.lock().unwrap() {
                        break;
                    }
                    sink.queue.lock().unwrap().push_back(message);
                    let window = HWND(raw as *mut core::ffi::c_void);
                    if unsafe {
                        PostMessageW(Some(window), WM_APP_OVERLAY_WAKE, WPARAM(0), LPARAM(0))
                    }
                    .is_err()
                    {
                        break;
                    }
                }
            });
        Self { shared }
    }
}

impl<T> Inbox<T> {
    /// Move every pending message out of the queue.
    pub fn drain(&self) -> Vec<T> {
        let mut queue = self.shared.queue.lock().unwrap();
        queue.drain(..).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.shared.queue.lock().unwrap().is_empty()
    }

    /// Stop accepting messages; used before the window is destroyed.
    pub fn close(&self) {
        *self.shared.closed.lock().unwrap() = true;
    }
}

impl<T> Drop for Inbox<T> {
    fn drop(&mut self) {
        self.close();
    }
}

/// True when opaque windows above `hwnd` cover its whole rectangle.
///
/// Layered and click-through windows are explicitly not occluders: the desktop
/// lyrics and the mini player composite arbitrary per-pixel alpha, so a window
/// underneath them may still be partly visible. Conservatively repainting a
/// little under such a window is much cheaper than leaving stale pixels.
pub fn fully_occluded(hwnd: HWND) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GW_HWNDPREV, GWL_EXSTYLE, GetWindow, GetWindowLongPtrW, GetWindowRect, IsIconic,
        IsWindowVisible, WS_EX_LAYERED, WS_EX_TRANSPARENT,
    };

    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return true;
        }
        let mut target = RECT::default();
        if GetWindowRect(hwnd, &mut target).is_err() {
            return false;
        }
        let full = (target.left, target.top, target.right, target.bottom);
        if full.2 <= full.0 || full.3 <= full.1 {
            return false;
        }
        let mut pieces = vec![full];
        let mut above = GetWindow(hwnd, GW_HWNDPREV).ok();
        let mut guard = 0;
        while let Some(window) = above {
            guard += 1;
            if guard > 64 {
                break;
            }
            if IsWindowVisible(window).as_bool() && !IsIconic(window).as_bool() {
                let style = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
                let transparent = style & (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) != 0;
                if !transparent {
                    let mut rect = RECT::default();
                    if GetWindowRect(window, &mut rect).is_ok() {
                        let cut = (rect.left, rect.top, rect.right, rect.bottom);
                        let mut next = Vec::with_capacity(pieces.len() * 2);
                        for piece in pieces.drain(..) {
                            subtract(piece, cut, &mut next);
                        }
                        pieces = next;
                        if pieces.is_empty() {
                            return true;
                        }
                        // Pathological z-orders degrade to "not occluded"
                        // rather than doing unbounded rectangle splitting.
                        if pieces.len() > 32 {
                            return false;
                        }
                    }
                }
            }
            above = GetWindow(window, GW_HWNDPREV).ok();
        }
        pieces.is_empty()
    }
}

fn subtract(
    rect: (i32, i32, i32, i32),
    cut: (i32, i32, i32, i32),
    out: &mut Vec<(i32, i32, i32, i32)>,
) {
    let left = rect.0.max(cut.0);
    let top = rect.1.max(cut.1);
    let right = rect.2.min(cut.2);
    let bottom = rect.3.min(cut.3);
    if right <= left || bottom <= top {
        out.push(rect);
        return;
    }
    if rect.0 < left {
        out.push((rect.0, rect.1, left, rect.3));
    }
    if right < rect.2 {
        out.push((right, rect.1, rect.2, rect.3));
    }
    if rect.1 < top {
        out.push((left, rect.1, right, top));
    }
    if bottom < rect.3 {
        out.push((left, bottom, right, rect.3));
    }
}

#[cfg(test)]
mod tests {
    use super::subtract;

    #[test]
    fn subtract_covers_rect_entirely() {
        let mut out = Vec::new();
        subtract((0, 0, 10, 10), (-5, -5, 15, 15), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn subtract_leaves_the_remaining_band() {
        let mut out = Vec::new();
        subtract((0, 0, 10, 10), (0, 0, 5, 10), &mut out);
        assert_eq!(out, vec![(5, 0, 10, 10)]);
    }

    #[test]
    fn non_intersecting_rect_is_kept() {
        let mut out = Vec::new();
        subtract((0, 0, 10, 10), (20, 20, 30, 30), &mut out);
        assert_eq!(out, vec![(0, 0, 10, 10)]);
    }
}

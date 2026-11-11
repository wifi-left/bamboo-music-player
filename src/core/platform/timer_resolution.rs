//! Process timer resolution for overlay animations.
//!
//! `SetTimer` runs on the default 15.6 ms system tick: a 60 fps request
//! (16 ms) really fires every ~31 ms, so the karaoke fill steps twice as
//! slowly as configured. Raise the process to 1 ms resolution only while a
//! high-frequency overlay animation is active, and drop it again on
//! pause/hide so an idle player does not keep the whole system on 1 ms
//! ticks.

use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};

/// RAII guard around `timeBeginPeriod(1)`/`timeEndPeriod(1)`.
///
/// Windows refcounts the request per process, so every overlay owns its own
/// guard and `set(false)`/`Drop` releases exactly the request it made.
pub struct TimerResolution {
    acquired: bool,
}

impl TimerResolution {
    pub const fn new() -> Self {
        Self { acquired: false }
    }

    /// Acquire or release the 1 ms resolution. No-op when unchanged.
    pub fn set(&mut self, acquired: bool) {
        if acquired == self.acquired {
            return;
        }
        self.acquired = acquired;
        unsafe {
            if acquired {
                timeBeginPeriod(1);
            } else {
                timeEndPeriod(1);
            }
        }
    }

    pub fn release(&mut self) {
        self.set(false);
    }
}

impl Default for TimerResolution {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        self.release();
    }
}

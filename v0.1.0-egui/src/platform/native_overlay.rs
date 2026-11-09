//! Host side of the in-process native desktop-lyrics overlay.
//!
//! The overlay owns a thread and a Win32 message loop, but lives in the same
//! executable/process as the player. State and menu/geometry actions travel
//! over channels, so no named pipe, helper executable or child process is
//! involved.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::overlay_protocol::{HostMessage, OverlayAction};

pub struct NativeOverlay {
    tx: Sender<HostMessage>,
    actions: Receiver<OverlayAction>,
    alive: Arc<AtomicBool>,
}

impl NativeOverlay {
    pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let (tx, messages) = unbounded::<HostMessage>();
        let (actions_tx, actions) = unbounded::<OverlayAction>();
        let alive = Arc::new(AtomicBool::new(true));
        let thread_alive = alive.clone();
        // The overlay thread exits by itself when it receives Shutdown (or
        // when the process exits); the player never blocks its UI joining it.
        let _ = crate::desktop_overlay::spawn(messages, actions_tx, thread_alive, wake)?;
        Ok(Self {
            tx,
            actions,
            alive,
        })
    }

    pub fn send(&self, message: HostMessage) {
        if self.alive.load(Ordering::Relaxed) {
            let _ = self.tx.send(message);
        }
    }

    pub fn take_actions(&self) -> Vec<OverlayAction> {
        let mut actions = Vec::new();
        while let Ok(action) = self.actions.try_recv() {
            actions.push(action);
        }
        actions
    }

    pub fn is_alive(&mut self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub fn shutdown(&mut self) {
        let _ = self.tx.send(HostMessage::Shutdown);
        self.alive.store(false, Ordering::Relaxed);
    }
}

impl Drop for NativeOverlay {
    fn drop(&mut self) {
        let _ = self.tx.send(HostMessage::Shutdown);
        self.alive.store(false, Ordering::Relaxed);
    }
}

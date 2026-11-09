//! Host side of the in-process native mini player.
//!
//! Like the desktop-lyrics overlay it owns a Win32 message loop and a
//! Direct2D renderer on its own thread, but lives in the player process. The
//! UI thread exchanges state and button/geometry actions over channels.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::overlay_protocol::{MiniAction, MiniHostMessage};

pub struct NativeMini {
    tx: Sender<MiniHostMessage>,
    actions: Receiver<MiniAction>,
    alive: Arc<AtomicBool>,
}

impl NativeMini {
    pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let (tx, messages) = unbounded::<MiniHostMessage>();
        let (actions_tx, actions) = unbounded::<MiniAction>();
        let alive = Arc::new(AtomicBool::new(true));
        let thread_alive = alive.clone();
        let _ = crate::mini_overlay::spawn(messages, actions_tx, thread_alive, wake)?;
        Ok(Self {
            tx,
            actions,
            alive,
        })
    }

    pub fn send(&self, message: MiniHostMessage) {
        if self.alive.load(Ordering::Relaxed) {
            let _ = self.tx.send(message);
        }
    }

    pub fn take_actions(&self) -> Vec<MiniAction> {
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
        let _ = self.tx.send(MiniHostMessage::Shutdown);
        self.alive.store(false, Ordering::Relaxed);
    }
}

impl Drop for NativeMini {
    fn drop(&mut self) {
        let _ = self.tx.send(MiniHostMessage::Shutdown);
        self.alive.store(false, Ordering::Relaxed);
    }
}

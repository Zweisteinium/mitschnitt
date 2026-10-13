//! Built-in monitoring: plays the capture on the default output while the app renders into a silent device.

use std::sync::atomic::{AtomicBool, Ordering};

pub use crate::platform::os::monitor::{Monitor, SUPPORTED};

#[derive(Default)]
pub struct MonitorState {
    /// Muted because the default output device changed.
    pub muted: AtomicBool,
    /// Accept the current default device as home.
    pub(crate) rehome: AtomicBool,
}

impl MonitorState {
    pub fn unmute(&self) {
        self.rehome.store(true, Ordering::Relaxed);
    }
}

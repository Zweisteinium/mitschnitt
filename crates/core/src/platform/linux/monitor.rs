//! Not needed on Linux: the app keeps playing on the user's output.

use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use crate::monitor::MonitorState;

pub const SUPPORTED: bool = false;

pub enum Monitor {}

impl Monitor {
    pub fn start(_rate: u32, _channels: u16, _gain: f32) -> Option<Monitor> {
        None
    }

    pub fn feed(&self, _data: &[f32], _frames: usize) {
        match *self {}
    }

    pub fn volume_handle(&self) -> Arc<AtomicU32> {
        match *self {}
    }

    pub fn state(&self) -> Arc<MonitorState> {
        match *self {}
    }
}

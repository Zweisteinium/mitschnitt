//! Per-app capture: the app's own audio as 32-bit float, before anything else is mixed in.

pub use crate::platform::os::capture::Capture;

/// One packet. `data` is empty when the app rendered nothing ([`crate::passive::FLAG_SILENT`]).
pub struct Packet {
    pub data: Vec<f32>,
    pub frames: usize,
    pub flags: u32,
}

pub enum CaptureEvent {
    Packet(Packet),
    Error(String),
}

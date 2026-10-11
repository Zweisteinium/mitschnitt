//! Not available on Linux yet: desktops route media keys and Bluetooth commands to an MPRIS player of their
//! own choosing.

use crate::media::Extra;
use crate::passive::Track;

pub const SUPPORTED: bool = false;

pub enum Guard {}

impl Guard {
    pub fn start() -> Result<Guard, String> {
        Err("not available on Linux".into())
    }

    pub fn update(&mut self, _recording: bool, _current: Option<&(Track, Extra)>) {
        match *self {}
    }

    pub fn blocked(&self) -> u32 {
        match *self {}
    }
}

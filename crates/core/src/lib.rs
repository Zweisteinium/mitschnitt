//! Mitschnitt core: everything that doesn't need a UI.
//!
//! Platform-neutral: [`passive`] (when a track starts, completes or is discarded), [`analyze`] (forensic checks),
//! [`flac`] and [`output`] (files), [`history`] (files written), [`profile`] (supported apps), [`settings`],
//! [`runner`] (playing a queue of songs), [`echolot`] (the Echolot API) and [`recorder`] (working through Echolot's or
//! an exported queue).
//! Per platform, behind the same API: [`capture`], [`devices`], [`media`], [`sessions`], [`setup`], [`monitor`],
//! [`guard`], [`power`], [`desktop`]; [`engine`] wires them to [`passive`].

pub mod analyze;
pub mod audio;
pub mod capture;
pub mod devices;
pub mod echolot;
pub mod engine;
pub mod flac;
pub mod history;
pub mod media;
pub mod monitor;
pub mod output;
pub mod passive;
pub mod platform;
pub mod profile;
pub mod recorder;
pub mod runner;
pub mod sessions;
pub mod settings;
pub mod setup;

pub use platform::os::{desktop, guard, init_thread, power};

/// `m:ss.mmm` for a frame index (rounded to the nearest millisecond).
pub fn timestamp(frame: usize, rate: u32) -> String {
    let ms = (frame as u64 * 1000 + rate as u64 / 2) / rate as u64;
    format!("{}:{:02}.{:03}", ms / 60_000, (ms / 1000) % 60, ms % 1000)
}

#[cfg(test)]
mod tests {
    #[test]
    fn timestamp_formats_minutes_seconds_millis() {
        assert_eq!(super::timestamp(0, 44_100), "0:00.000");
        assert_eq!(super::timestamp(4_587_723, 44_100), "1:44.030");
    }
}

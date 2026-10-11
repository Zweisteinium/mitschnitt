//! Mitschnitt core: everything that doesn't need a UI.
//!
//! Platform-neutral (unit-tested):
//! - [`audio`]: PCM in memory
//! - [`passive`]: the passive-mode state machine (when a track starts, completes or is discarded)
//! - [`analyze`]: forensic checks on a finished recording (dropouts, Windows limiter, fades, ...)
//! - [`flac`]: reading and verified writing of FLAC files
//! - [`profile`]: the supported apps and everything app-specific
//!
//! Windows adapters: [`devices`], [`capture`] (process loopback), [`sessions`] (app volume),
//! [`media`] (SMTC), and [`engine`], which wires them to [`passive`].

pub mod analyze;
pub mod audio;
pub mod flac;
pub mod output;
pub mod passive;
pub mod profile;
pub mod settings;

#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod devices;
#[cfg(windows)]
pub mod engine;
#[cfg(windows)]
pub mod keys;
#[cfg(windows)]
pub mod media;
#[cfg(windows)]
pub mod monitor;
#[cfg(windows)]
pub mod own_session;
#[cfg(windows)]
pub mod sessions;
#[cfg(windows)]
pub mod setup;

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

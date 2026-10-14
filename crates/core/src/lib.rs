//! Mitschnitt core: everything that doesn't need a UI.

pub mod analyze;
pub mod audio;
pub mod capture;
pub mod devices;
pub mod echolot;
pub mod engine;
pub mod flac;
pub mod history;
pub mod log;
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
pub mod spectrum;

pub use platform::os::{desktop, guard, init_thread, power};

/// At most one decimal, none when it is zero: `12`, `31.5`, `-0.1`.
pub fn number(x: f64) -> String {
    let s = format!("{x:.1}");
    let s = s.strip_suffix(".0").unwrap_or(&s);
    if s == "-0" { "0".into() } else { s.to_owned() }
}

/// A length for people: milliseconds below a second (`6 ms`, `31.5 ms`), else seconds (`1.2 s`).
pub fn duration(ms: f64) -> String {
    if ms < 1000.0 { format!("{} ms", number(ms)) } else { format!("{} s", number(ms / 1000.0)) }
}

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

    #[test]
    fn numbers_drop_a_zero_decimal() {
        assert_eq!([super::number(12.0), super::number(31.54), super::number(-0.04), super::number(-0.1)], ["12", "31.5", "0", "-0.1"]);
        assert_eq!([super::duration(6.0), super::duration(11.61), super::duration(1_250.0)], ["6 ms", "11.6 ms", "1.2 s"]);
    }
}

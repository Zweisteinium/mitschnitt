//! A plain event log for finding out afterwards what happened in a run (what the runner did, why a song ended).
//! Only what is passed to [`log!`] lands in it: never tokens, nothing from the libraries.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use time::{OffsetDateTime, UtcOffset, macros::format_description};

const MAX_SIZE: u64 = 1_000_000;

static LOG: OnceLock<(Mutex<File>, UtcOffset)> = OnceLock::new();

pub fn path() -> PathBuf {
    crate::settings::state_dir().join("mitschnitt.log")
}

/// Before other threads start: the local offset can't be read later on Linux. Above 1 MB the file becomes
/// `mitschnitt.old.log`.
pub fn init(who: &str) {
    let path = path();
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_SIZE) {
        let _ = std::fs::rename(&path, path.with_file_name("mitschnitt.old.log"));
    }
    let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) else { return };
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    if LOG.set((Mutex::new(file), offset)).is_ok() {
        line(format_args!("--- {who} {} pid {}", env!("CARGO_PKG_VERSION"), std::process::id()));
    }
}

/// Written at once, so a crash keeps everything before it.
pub fn line(args: std::fmt::Arguments) {
    let Some((file, offset)) = LOG.get() else { return };
    let at = OffsetDateTime::now_utc().to_offset(*offset);
    let stamp = at.format(format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]")).unwrap_or_default();
    let thread = std::thread::current();
    let text = format!("{stamp} {:<14} {args}\n", thread.name().unwrap_or("-"));
    if let Ok(mut file) = file.lock() {
        let _ = file.write_all(text.as_bytes());
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::line(format_args!($($arg)*)) };
}

//! The player's media session: which supported apps run, and live snapshots of one.

pub use crate::platform::os::media::{Watcher, find, sources};

use crate::flac::Cover;

/// A supported app and what it is doing right now.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Source {
    pub id: &'static str,
    pub name: &'static str,
    pub running: bool,
    pub playing: bool,
    pub title: String,
    pub artist: String,
}

/// Tags beyond the track identity, read once when a recording starts.
#[derive(Debug, Clone, Default)]
pub struct Extra {
    pub album_artist: String,
    pub track_number: u32,
    pub cover: Option<Cover>,
}

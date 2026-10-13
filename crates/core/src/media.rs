//! The player's media session: which supported apps run, and live snapshots of one.

pub use crate::platform::os::media::{Watcher, find, sources};

use crate::flac::Cover;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Source {
    pub id: &'static str,
    pub name: &'static str,
    pub running: bool,
    pub playing: bool,
    pub title: String,
    pub artist: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Play,
    Pause,
    Next,
    Previous,
    /// To the start of the current track.
    Restart,
}

/// Tags beyond the track identity, read once when a recording starts.
#[derive(Debug, Clone, Default)]
pub struct Extra {
    pub album_artist: String,
    pub track_number: u32,
    pub cover: Option<Cover>,
}

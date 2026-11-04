//! The player's media session: which supported apps run, and live snapshots of one.

pub use crate::platform::os::media::{Watcher, find, sources};

use crate::flac::Cover;
use crate::profile::Profile;

/// What an app's player shows outside a recording (ads left out), with its cover when asked: reading it costs.
pub fn now_playing(profile: &Profile, cover: bool) -> Option<(crate::passive::Media, Option<Cover>)> {
    let mut watcher = Watcher::new(find(profile).ok().flatten()?);
    let mut media = watcher.snapshot().ok()?;
    media.track = media.track.filter(|t| !(profile.is_ad)(t));
    let cover = if cover && media.track.is_some() { watcher.extra().cover } else { None };
    Some((media, cover))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Source {
    pub id: &'static str,
    pub name: &'static str,
    pub running: bool,
    pub playing: bool,
    pub title: String,
    pub artist: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Repeat {
    Off,
    Track,
    List,
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

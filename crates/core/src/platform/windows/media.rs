//! Windows media controls (SMTC): which supported apps are running, and live snapshots of one.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::DataReader;
use windows::core::Result;

use crate::flac::Cover;
use crate::media::{Control, Extra, Source};
use crate::passive::{Media, Track};
use crate::profile::{PROFILES, Profile};
use crate::sessions;

pub fn manager() -> Result<Manager> {
    Manager::RequestAsync()?.join()
}

/// Every supported app, running or not.
pub fn sources() -> Result<Vec<Source>> {
    let procs = sessions::processes();
    let sessions = manager()?.GetSessions()?;
    let mut out = Vec::new();
    for p in PROFILES {
        let mut source = Source { id: p.id, name: p.name, running: false, playing: false, title: String::new(), artist: String::new() };
        source.running = procs.iter().any(|proc| p.matches_exe(&proc.name));
        for session in &sessions {
            if source.running && p.matches_media(&session.SourceAppUserModelId()?.to_string()) {
                let props = session.TryGetMediaPropertiesAsync()?.join()?;
                source.playing = session.GetPlaybackInfo()?.PlaybackStatus()? == PlaybackStatus::Playing;
                source.title = props.Title()?.to_string();
                source.artist = props.Artist()?.to_string();
            }
        }
        out.push(source);
    }
    Ok(out)
}

/// The media session of `profile`'s app.
pub fn find(profile: &Profile) -> Result<Option<Session>> {
    for session in manager()?.GetSessions()? {
        if profile.matches_media(&session.SourceAppUserModelId()?.to_string()) {
            return Ok(Some(session));
        }
    }
    Ok(None)
}

/// Polls one session cheaply: playback state and timeline every call, titles only when the track
/// length changes or every [`TITLE_REFRESH`].
///
/// Spotify publishes the new length ~0.3-0.4 s before the new title. Until the title catches up (or
/// [`SETTLE`] passes - the next track may share its title), no track is reported, so a recording never
/// starts under the previous track's name.
pub struct Watcher {
    session: Session,
    track: Option<Track>,
    titles_at: Instant,
    /// The length changed but the title didn't (yet): since when, and the title it still shows.
    unsettled: Option<(Instant, String, String)>,
}

const TITLE_REFRESH: Duration = Duration::from_millis(400);
const SETTLE: Duration = Duration::from_millis(1_500);

impl Watcher {
    pub fn new(session: Session) -> Watcher {
        Watcher { session, track: None, titles_at: Instant::now() - TITLE_REFRESH, unsettled: None }
    }

    pub fn snapshot(&mut self) -> Result<Media> {
        let playing = self.session.GetPlaybackInfo()?.PlaybackStatus()? == PlaybackStatus::Playing;
        let timeline = self.session.GetTimelineProperties()?;
        let duration_ms = ((timeline.EndTime()?.Duration - timeline.StartTime()?.Duration) / 10_000).max(0) as u64;
        let mut position_ms = timeline.Position()?.Duration / 10_000;
        if playing {
            // The timeline is a snapshot from `LastUpdatedTime`; extrapolate to now.
            position_ms += ((filetime_now() - timeline.LastUpdatedTime()?.UniversalTime) / 10_000).max(0);
        }
        let length_changed = self.track.as_ref().is_some_and(|t| t.duration_ms != duration_ms);
        let stale = self.track.is_none() || length_changed || self.unsettled.is_some() || self.titles_at.elapsed() >= TITLE_REFRESH;
        if stale {
            let props = self.session.TryGetMediaPropertiesAsync()?.join()?;
            let track = Track { title: props.Title()?.to_string(), artist: props.Artist()?.to_string(), album: props.AlbumTitle()?.to_string(), duration_ms };
            if length_changed {
                let old = self.track.as_ref().map(|t| (t.title.clone(), t.artist.clone())).unwrap_or_default();
                if (track.title.clone(), track.artist.clone()) == old {
                    self.unsettled.get_or_insert((Instant::now(), old.0, old.1));
                }
            }
            if let Some((since, title, artist)) = &self.unsettled {
                if track.title != *title || track.artist != *artist || since.elapsed() >= SETTLE {
                    self.unsettled = None;
                }
            }
            self.track = Some(track);
            self.titles_at = Instant::now();
        }
        let track = self.track.clone().filter(|t| self.unsettled.is_none() && !t.title.is_empty() && t.duration_ms > 0);
        Ok(Media { playing, track, position_ms })
    }
}

impl Watcher {
    /// Send a command to the player.
    pub fn control(&self, control: Control) -> std::result::Result<(), String> {
        let op = match control {
            Control::Pause => self.session.TryPauseAsync(),
            Control::Next => self.session.TrySkipNextAsync(),
            Control::Previous => self.session.TrySkipPreviousAsync(),
        };
        op.and_then(|op| op.join()).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Album artist, track number and cover of what is playing now (best effort).
    pub fn extra(&self) -> Extra {
        let Ok(props) = self.session.TryGetMediaPropertiesAsync().and_then(|op| op.join()) else { return Extra::default() };
        Extra {
            album_artist: props.AlbumArtist().map(|s| s.to_string()).unwrap_or_default(),
            track_number: props.TrackNumber().unwrap_or(0).max(0) as u32,
            cover: props.Thumbnail().ok().and_then(|t| read_cover(&t).ok().flatten()),
        }
    }
}

fn read_cover(thumbnail: &windows::Storage::Streams::IRandomAccessStreamReference) -> Result<Option<Cover>> {
    let stream = thumbnail.OpenReadAsync()?.join()?;
    let size = stream.Size()?.min(16 << 20) as u32;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
    reader.LoadAsync(size)?.join()?;
    let mut data = vec![0u8; size as usize];
    reader.ReadBytes(&mut data)?;
    let mime = match data.as_slice() {
        [0xFF, 0xD8, ..] => "image/jpeg",
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        _ => return Ok(None),
    };
    Ok(Some(Cover { mime, data }))
}

/// Now as a Windows FILETIME (100 ns since 1601), the clock SMTC timestamps use.
fn filetime_now() -> i64 {
    const UNIX_TO_FILETIME: i64 = 116_444_736_000_000_000;
    let since_unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    UNIX_TO_FILETIME + (since_unix.as_nanos() / 100) as i64
}

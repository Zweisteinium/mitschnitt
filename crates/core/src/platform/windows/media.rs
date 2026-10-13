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

pub fn find(profile: &Profile) -> Result<Option<Session>> {
    for session in manager()?.GetSessions()? {
        if profile.matches_media(&session.SourceAppUserModelId()?.to_string()) {
            return Ok(Some(session));
        }
    }
    Ok(None)
}

/// Titles are read only when the length changes or every [`TITLE_REFRESH`]. Spotify publishes the new length ~0.3 s
/// before the new title: no track is reported until the title catches up (or [`SETTLE`] passes), so a recording never
/// starts under the previous track's name.
pub struct Watcher {
    session: Session,
    track: Option<Track>,
    titles_at: Instant,
    /// The length changed but the title didn't (yet): since when, and the title it still shows.
    unsettled: Option<(Instant, String, String)>,
    clock: Clock,
}

/// Extrapolates the timeline (a position at `LastUpdatedTime`) only from when the snapshot or the playback was first
/// seen: a snapshot published while the song was loading or paused made a fresh start look seconds in ("joined
/// mid-track" on first tries).
#[derive(Default)]
struct Clock {
    snapshot: Option<((i64, i64), Instant)>,
    playing: Option<(bool, Instant)>,
}

impl Clock {
    /// `snapshot`: (position ms, update stamp); `age_ms`: how old the player says it is.
    fn position(&mut self, now: Instant, playing: bool, snapshot: (i64, i64), age_ms: i64) -> i64 {
        // The first look can't tell when anything began: the player's own age is all there is.
        let first = now.checked_sub(Duration::from_millis(age_ms.max(0) as u64)).unwrap_or(now);
        let snapshot_seen = match self.snapshot {
            Some((s, at)) if s == snapshot => at,
            Some(_) => now,
            None => first,
        };
        let playing_seen = match self.playing {
            Some((p, at)) if p == playing => at,
            Some(_) => now,
            None => first,
        };
        self.snapshot = Some((snapshot, snapshot_seen));
        self.playing = Some((playing, playing_seen));
        if !playing {
            return snapshot.0;
        }
        let since = now.duration_since(snapshot_seen.max(playing_seen)).as_millis() as i64;
        snapshot.0 + age_ms.min(since).max(0)
    }
}

const TITLE_REFRESH: Duration = Duration::from_millis(400);
const SETTLE: Duration = Duration::from_millis(1_500);

impl Watcher {
    pub fn new(session: Session) -> Watcher {
        Watcher { session, track: None, titles_at: Instant::now() - TITLE_REFRESH, unsettled: None, clock: Clock::default() }
    }

    pub fn snapshot(&mut self) -> Result<Media> {
        let playing = self.session.GetPlaybackInfo()?.PlaybackStatus()? == PlaybackStatus::Playing;
        let timeline = self.session.GetTimelineProperties()?;
        let duration_ms = ((timeline.EndTime()?.Duration - timeline.StartTime()?.Duration) / 10_000).max(0) as u64;
        let position = timeline.Position()?.Duration / 10_000;
        let updated = timeline.LastUpdatedTime()?.UniversalTime;
        let age_ms = (filetime_now() - updated) / 10_000;
        let position_ms = self.clock.position(Instant::now(), playing, (position, updated), age_ms);
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
            if let Some((since, title, artist)) = &self.unsettled
                && (track.title != *title || track.artist != *artist || since.elapsed() >= SETTLE)
            {
                self.unsettled = None;
            }
            self.track = Some(track);
            self.titles_at = Instant::now();
        }
        let track = self.track.clone().filter(|t| self.unsettled.is_none() && !t.title.is_empty() && t.duration_ms > 0);
        Ok(Media { playing, track, position_ms })
    }
}

impl Watcher {
    pub fn control(&self, control: Control) -> std::result::Result<(), String> {
        let op = match control {
            Control::Play => self.session.TryPlayAsync(),
            Control::Pause => self.session.TryPauseAsync(),
            Control::Next => self.session.TrySkipNextAsync(),
            Control::Previous => self.session.TrySkipPreviousAsync(),
            Control::Restart => self.session.TryChangePlaybackPositionAsync(0),
        };
        op.and_then(|op| op.join()).map(|_| ()).map_err(|e| e.to_string())
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_snapshot_from_before_the_start_does_not_count_as_played() {
        let mut clock = Clock::default();
        let t0 = Instant::now();
        // Paused (or loading) at 0:00; the player refreshed the snapshot 1 s before the first look.
        assert_eq!(clock.position(t0, false, (0, 100), 1_000), 0);
        // Playing 4 s later with the same snapshot, now 5 s old: only the 300 ms since the start are played.
        assert_eq!(clock.position(t0 + 4_000 * MS, true, (0, 100), 5_000), 0);
        assert_eq!(clock.position(t0 + 4_300 * MS, true, (0, 100), 5_300), 300);
        // The player's next snapshot takes over.
        assert_eq!(clock.position(t0 + 6_000 * MS, true, (2_000, 200), 20), 2_000);
        assert_eq!(clock.position(t0 + 7_000 * MS, true, (2_000, 200), 1_020), 3_000);
    }

    #[test]
    fn a_running_song_is_extrapolated_by_the_players_age() {
        let mut clock = Clock::default();
        let t0 = Instant::now();
        // First look in the middle of a song: nothing better than the player's own age.
        assert_eq!(clock.position(t0, true, (60_000, 100), 2_000), 62_000);
        assert_eq!(clock.position(t0 + 500 * MS, true, (60_000, 100), 2_500), 62_500);
    }

    #[test]
    fn a_clock_that_runs_off_is_capped() {
        let mut clock = Clock::default();
        let t0 = Instant::now();
        assert_eq!(clock.position(t0, true, (10_000, 100), 50), 10_050);
        // A new snapshot stamped 4 s in the past (the player's clock is off): counted from when it was seen.
        assert_eq!(clock.position(t0 + 2_000 * MS, true, (12_000, 200), 4_000), 12_000);
        assert_eq!(clock.position(t0 + 3_000 * MS, true, (12_000, 200), 5_000), 13_000);
    }
}

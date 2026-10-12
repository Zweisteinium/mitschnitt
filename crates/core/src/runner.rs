//! Queue runner: plays requested songs one after another in the player and records each one.
//!
//! For every [`Job`] it opens the song's link (`spotify:track:<id>:play` starts it at 0:00), checks that the right
//! song plays (correcting an album that starts one track off with next/previous), lets [`crate::passive`] record
//! it, pauses the player at the exact end (otherwise the album plays on) and waits a moment of silence before the
//! next song, so every song gets a clean start. Anything the runner didn't cause - pause, skip, seek, volume,
//! another song - means the user took over.
//!
//! Pure logic like `passive`: fed with media snapshots, `passive` events and write results, it answers with
//! [`Action`]s for the player and finished [`Outcome`]s, so it is fully unit-testable.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::passive::{Event, Media, Reason, Track};

/// Silence between the end of one song (player paused) and the start of the next.
const GAP: Duration = Duration::from_millis(1_200);
/// How long the player may take to start a song (a player that is still launching needs longer).
const START_TIMEOUT: Duration = Duration::from_secs(12);
const FIRST_START_TIMEOUT: Duration = Duration::from_secs(25);
/// After a next/previous, how long to wait for the player to show another song.
const CORRECTION_TIMEOUT: Duration = Duration::from_secs(4);
/// The right song plays, but no recording started: no sound reaches this PC.
const SOUND_TIMEOUT: Duration = Duration::from_secs(6);
/// A song only counts as started from the beginning below this position.
const MAX_START_POSITION_MS: i64 = 3_000;
const LENGTH_TOLERANCE_MS: u64 = 2_000;
/// A song handed over while the user plays something himself waits this long for the player to be idle.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);
/// Corrections tried when another song plays: next, then back past the original to the previous one.
const CORRECTIONS: [Action; 3] = [Action::Next, Action::Previous, Action::Previous];

/// One song to record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Job {
    /// Echolot's item id (`None` for songs from an exported list).
    pub id: Option<u64>,
    /// `spotify:track:<id>`.
    pub uri: String,
    pub artist: String,
    pub artists: Vec<String>,
    pub title: String,
    pub album: String,
    pub duration_ms: u64,
    pub isrc: String,
}

impl Job {
    /// The track id from the uri (`spotify:track:<id>` -> `<id>`).
    pub fn track_id(&self) -> &str {
        self.uri.rsplit(':').next().unwrap_or_default()
    }

    /// The link that starts this song at 0:00.
    pub fn play_uri(&self) -> String {
        format!("{}:play", self.uri.trim_end_matches(":play"))
    }

    pub fn label(&self) -> String {
        format!("{} - {}", self.artist, self.title)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open a link with the desktop (starts the player if needed).
    Open,
    Pause,
    Next,
    Previous,
}

/// How a job ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Recorded and written; `note` explains anything that isn't bit-exact (a faded-in start).
    Recorded { file: PathBuf, note: Option<String> },
    /// The player delivered no lossless stream.
    LossyOnly { detail: String },
    WrongSong { detail: String },
    /// The user paused, skipped, seeked, changed the volume or played something else.
    TookOver { detail: String },
    /// Buffering, a capture glitch, no clean start: worth another try.
    Stalled { detail: String },
    Failed { detail: String },
    /// The right song plays but no sound reaches this PC (Spotify Connect playing on another device?).
    NoSound,
}

/// What became of a finished recording, reported by whoever writes the file.
#[derive(Debug, Clone, PartialEq)]
pub enum Written {
    Saved { file: PathBuf, note: Option<String> },
    /// Not on the lossless grid: the stream wasn't lossless (or was processed).
    Lossy { detail: String },
    Failed { error: String },
}

#[derive(Debug)]
enum Stage {
    /// Ready for the next job once the gap has passed.
    Idle { since: Instant },
    /// The user plays something himself: wait before starting the job.
    Waiting { job: Job, since: Instant },
    /// The link was opened; waiting for the player to show the song.
    Starting { job: Job, at: Instant, timeout: Duration, corrections: usize, wrong: Option<Track>, retried: bool },
    /// The right song plays; waiting for the recording to start.
    Playing { job: Job, at: Instant, retried: bool },
    Recording { job: Job, at: Instant, retried: bool },
    /// Complete; the file is being written.
    Writing { job: Job },
    /// Start the same song again after the gap (the first try was spoilt by the runner's own start-up).
    Reopen { job: Job, since: Instant },
}

pub struct Runner {
    stage: Stage,
    /// No song was started yet (the player may still be launching).
    first: bool,
    done: Vec<(Job, Outcome)>,
}

impl Runner {
    pub fn new(now: Instant) -> Runner {
        Runner { stage: Stage::Idle { since: now }, first: true, done: Vec::new() }
    }

    /// Ready for a new job: nothing running and the gap after the last song has passed.
    pub fn ready(&self, now: Instant) -> bool {
        matches!(self.stage, Stage::Idle { since } if now >= since + GAP)
    }

    /// The job being worked on.
    pub fn job(&self) -> Option<&Job> {
        match &self.stage {
            Stage::Idle { .. } => None,
            Stage::Waiting { job, .. } | Stage::Starting { job, .. } | Stage::Playing { job, .. } | Stage::Recording { job, .. } | Stage::Writing { job } | Stage::Reopen { job, .. } => Some(job),
        }
    }

    /// The job's song is being recorded right now.
    pub fn recording(&self) -> bool {
        matches!(self.stage, Stage::Recording { .. })
    }

    /// Start `job` (returns the action that opens its link), or wait while the player plays something else.
    pub fn assign(&mut self, job: Job, now: Instant, playing: bool) -> Vec<Action> {
        if playing {
            self.stage = Stage::Waiting { job, since: now };
            return Vec::new();
        }
        self.open(job, now)
    }

    fn open(&mut self, job: Job, now: Instant) -> Vec<Action> {
        let timeout = if self.first { FIRST_START_TIMEOUT } else { START_TIMEOUT };
        self.first = false;
        self.stage = Stage::Starting { job, at: now, timeout, corrections: 0, wrong: None, retried: false };
        vec![Action::Open]
    }

    /// Finished jobs since the last call.
    pub fn take_done(&mut self) -> Vec<(Job, Outcome)> {
        std::mem::take(&mut self.done)
    }

    /// Give up the current job with `outcome` (the engine stops). A song being written is left to its writer.
    pub fn abort(&mut self, now: Instant, outcome: Outcome) -> Vec<Action> {
        match self.job().cloned() {
            Some(job) if !self.writing() => self.finish(job, outcome, now, true),
            _ => Vec::new(),
        }
    }

    /// The current job's song is complete and its file is being written.
    pub fn writing(&self) -> bool {
        matches!(self.stage, Stage::Writing { .. })
    }

    fn finish(&mut self, job: Job, outcome: Outcome, now: Instant, pause: bool) -> Vec<Action> {
        self.done.push((job, outcome));
        self.stage = Stage::Idle { since: now };
        if pause { vec![Action::Pause] } else { Vec::new() }
    }

    /// A media snapshot (10x per second).
    pub fn tick(&mut self, now: Instant, media: &Media) -> Vec<Action> {
        match &mut self.stage {
            Stage::Idle { .. } | Stage::Writing { .. } | Stage::Recording { .. } => Vec::new(),
            Stage::Waiting { job, since } => {
                let job = job.clone();
                if !media.playing {
                    return self.open(job, now);
                }
                if now.duration_since(*since) < BUSY_TIMEOUT {
                    return Vec::new();
                }
                self.finish(job, Outcome::TookOver { detail: "Spotify is in use".into() }, now, false)
            }
            Stage::Reopen { job, since } => {
                if now < *since + GAP {
                    return Vec::new();
                }
                let job = job.clone();
                self.stage = Stage::Starting { job, at: now, timeout: START_TIMEOUT, corrections: 0, wrong: None, retried: true };
                vec![Action::Open]
            }
            Stage::Playing { job, at, retried } => {
                // Paused before the recording began (a key pressed while the player had the focus): once more.
                if !media.playing && media.track.is_some() {
                    let (job, retried) = (job.clone(), *retried);
                    if !retried {
                        self.stage = Stage::Reopen { job, since: now };
                        return Vec::new();
                    }
                    return self.finish(job, Outcome::TookOver { detail: "Paused".into() }, now, false);
                }
                if now.duration_since(*at) < SOUND_TIMEOUT {
                    return Vec::new();
                }
                let job = job.clone();
                self.finish(job, Outcome::NoSound, now, true)
            }
            Stage::Starting { job, at, timeout, corrections, wrong, retried } => {
                let showing = media.track.as_ref().filter(|_| media.playing);
                let fresh = showing.filter(|t| wrong.as_ref() != Some(*t));
                let Some(track) = fresh else {
                    if now.duration_since(*at) < if wrong.is_some() { CORRECTION_TIMEOUT } else { *timeout } {
                        return Vec::new();
                    }
                    let job = job.clone();
                    // A correction that didn't change the song counts as another wrong song.
                    if let Some(w) = wrong.clone() {
                        return self.correct(job, w, now);
                    }
                    return self.finish(job, Outcome::Failed { detail: "the player didn't start the song".into() }, now, true);
                };
                if media.position_ms > MAX_START_POSITION_MS && wrong.is_none() && *corrections == 0 {
                    // Still the previous state of the player (or it resumed somewhere): wait for the start.
                    if now.duration_since(*at) < *timeout {
                        return Vec::new();
                    }
                    let job = job.clone();
                    return self.finish(job, Outcome::Failed { detail: format!("the song started at {} s instead of 0:00", media.position_ms / 1000) }, now, true);
                }
                match matches(job, track) {
                    Ok(()) => {
                        let (job, retried) = (job.clone(), *retried);
                        self.stage = Stage::Playing { job, at: now, retried };
                        Vec::new()
                    }
                    Err(_) => {
                        let (job, track) = (job.clone(), track.clone());
                        self.correct(job, track, now)
                    }
                }
            }
        }
    }

    /// Another song plays: try the next correction, or give up.
    fn correct(&mut self, job: Job, wrong: Track, now: Instant) -> Vec<Action> {
        let (corrections, retried) = match &self.stage {
            Stage::Starting { corrections, retried, .. } => (*corrections, *retried),
            _ => (0, false),
        };
        match CORRECTIONS.get(corrections) {
            Some(action) => {
                self.stage = Stage::Starting { job, at: now, timeout: START_TIMEOUT, corrections: corrections + 1, wrong: Some(wrong), retried };
                vec![*action]
            }
            None => {
                let detail = format!("\"{} - {}\" played instead, also after going back and forward", wrong.artist, wrong.title);
                self.finish(job, Outcome::WrongSong { detail }, now, true)
            }
        }
    }

    /// An event of the passive recorder.
    pub fn event(&mut self, now: Instant, event: &Event) -> Vec<Action> {
        match event {
            Event::Started(track) => {
                let target = match &self.stage {
                    Stage::Starting { job, retried, .. } | Stage::Playing { job, retried, .. } => matches(job, track).is_ok().then(|| (job.clone(), *retried)),
                    _ => None,
                };
                if let Some((job, retried)) = target {
                    self.stage = Stage::Recording { job, at: now, retried };
                }
                Vec::new()
            }
            Event::Dropped { track, reason } => {
                let (job, at, retried) = match &self.stage {
                    Stage::Recording { job, at, retried } if matches(job, track).is_ok() => (job.clone(), *at, *retried),
                    // Before the recording started: only a start that can never be clean ends the job.
                    Stage::Playing { job, at, retried } if matches(job, track).is_ok() && matches!(reason, Reason::MidTrack | Reason::NoCleanStart) => (job.clone(), *at, *retried),
                    _ => return Vec::new(),
                };
                // Spoilt by the runner's own start (a stream that appeared at the wrong volume, no clean start):
                // play the song once more from silence.
                // A pause right after the start is most likely a key that reached the player while it had the focus.
                let startup = matches!(reason, Reason::VolumeChanged | Reason::NoCleanStart | Reason::MidTrack | Reason::Paused) && now.duration_since(at) < Duration::from_secs(4);
                if startup && !retried {
                    self.stage = Stage::Reopen { job, since: now };
                    return vec![Action::Pause];
                }
                let detail = reason.message().to_owned();
                let outcome = match reason {
                    Reason::Paused | Reason::Seeked | Reason::TrackChanged | Reason::VolumeChanged => Outcome::TookOver { detail },
                    Reason::Underrun | Reason::Glitch | Reason::Stopped | Reason::MidTrack | Reason::NoCleanStart => Outcome::Stalled { detail },
                };
                // After a take-over the player is the user's: leave it alone.
                let pause = !matches!(outcome, Outcome::TookOver { .. });
                self.finish(job, outcome, now, pause)
            }
            Event::Completed { track, .. } => {
                if let Stage::Recording { job, .. } = &self.stage
                    && matches(job, track).is_ok()
                {
                    let job = job.clone();
                    self.stage = Stage::Writing { job };
                    // At the exact end: stop the player before the album plays on.
                    return vec![Action::Pause];
                }
                Vec::new()
            }
        }
    }

    /// Whether a completed recording belongs to the current job (it is then written for it).
    pub fn is_target(&self, track: &Track) -> bool {
        matches!(&self.stage, Stage::Writing { job } if matches(job, track).is_ok())
    }

    /// The file of the current job was written (or not).
    pub fn written(&mut self, now: Instant, result: Written) {
        if let Stage::Writing { job } = &self.stage {
            let job = job.clone();
            self.finish(job, result.into(), now, false);
        }
    }
}

impl From<Written> for Outcome {
    fn from(result: Written) -> Outcome {
        match result {
            Written::Saved { file, note } => Outcome::Recorded { file, note },
            Written::Lossy { detail } => Outcome::LossyOnly { detail },
            Written::Failed { error } => Outcome::Failed { detail: error },
        }
    }
}

/// Lower case letters and digits only (umlauts and other letters kept), single spaces.
fn norm(s: &str) -> String {
    let s: String = s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The title without "(feat. …)", "[…]", "(… Remix)" or " - Radio Edit" style additions.
fn base_title(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.split(" - ").next().unwrap_or_default().to_owned();
    let lower = out.to_lowercase();
    let cut = [" feat. ", " feat ", " ft. ", " featuring "].iter().filter_map(|f| lower.find(f)).min();
    norm(cut.map_or(out.as_str(), |i| &out[..i]))
}

/// Whether `track` (what the player shows) is the job's song: title (case and additions like "feat." or a version
/// suffix tolerated), one of the artists, and the length within two seconds.
pub fn matches(job: &Job, track: &Track) -> Result<(), String> {
    let (a, b) = (norm(&job.title), norm(&track.title));
    if a != b && (base_title(&job.title).is_empty() || base_title(&job.title) != base_title(&track.title)) {
        return Err(format!("title is \"{}\", expected \"{}\"", track.title, job.title));
    }
    let shown = norm(&track.artist);
    let artist_ok = std::iter::once(&job.artist).chain(&job.artists).map(|n| norm(n)).any(|n| !n.is_empty() && (shown.contains(&n) || n.contains(&shown)));
    if !artist_ok && !shown.is_empty() {
        return Err(format!("artist is \"{}\", expected \"{}\"", track.artist, job.artist));
    }
    if job.duration_ms > 0 && track.duration_ms.abs_diff(job.duration_ms) > LENGTH_TOLERANCE_MS {
        return Err(format!("length is {} s, expected {} s", track.duration_ms / 1000, job.duration_ms / 1000));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Job {
        Job {
            id: Some(123),
            uri: "spotify:track:6fhlcLPTrpYfPXRT0pwnxk".into(),
            artist: "NYCO".into(),
            artists: vec!["NYCO".into()],
            title: "Feel It".into(),
            album: "Feel It".into(),
            duration_ms: 225_000,
            isrc: "DEUM72401234".into(),
        }
    }

    fn track(title: &str, artist: &str, ms: u64) -> Track {
        Track { title: title.into(), artist: artist.into(), album: String::new(), duration_ms: ms }
    }

    fn playing(t: &Track, pos: i64) -> Media {
        Media { playing: true, track: Some(t.clone()), position_ms: pos }
    }

    #[test]
    fn job_links() {
        assert_eq!(job().track_id(), "6fhlcLPTrpYfPXRT0pwnxk");
        assert_eq!(job().play_uri(), "spotify:track:6fhlcLPTrpYfPXRT0pwnxk:play");
    }

    #[test]
    fn matching_tolerates_case_feat_and_versions_but_not_length() {
        let j = job();
        assert!(matches(&j, &track("feel it", "NYCO, Someone", 225_900)).is_ok());
        assert!(matches(&j, &track("Feel It (feat. Someone)", "NYCO", 225_000)).is_ok());
        assert!(matches(&j, &track("Feel It - Radio Edit", "NYCO", 224_000)).is_ok());
        assert!(matches(&j, &track("Feel It", "NYCO", 180_000)).is_err(), "another version has another length");
        assert!(matches(&j, &track("Other Song", "NYCO", 225_000)).is_err());
        assert!(matches(&j, &track("Feel It", "Somebody Else", 225_000)).is_err());
        let umlaut = Job { title: "Probier's mal mit Gemütlichkeit".into(), artist: "Edgar Ott".into(), ..job() };
        assert!(matches(&umlaut, &track("Probier’s mal mit Gemütlichkeit", "Edgar Ott", 225_000)).is_ok());
    }

    #[test]
    fn records_the_right_song_and_pauses_at_the_end() {
        let t0 = Instant::now();
        let mut r = Runner::new(t0);
        assert!(!r.ready(t0));
        let t = t0 + GAP;
        assert!(r.ready(t));
        assert_eq!(r.assign(job(), t, false), vec![Action::Open]);
        let song = track("Feel It", "NYCO", 225_000);
        assert!(r.tick(t, &Media::default()).is_empty(), "nothing plays yet");
        assert!(r.tick(t + Duration::from_secs(1), &playing(&song, 200)).is_empty());
        r.event(t + Duration::from_secs(1), &Event::Started(song.clone()));
        assert!(r.recording());
        let end = Event::Completed { track: song.clone(), samples: vec![], off_grid: 0, off_span: None };
        assert_eq!(r.event(t + Duration::from_secs(226), &end), vec![Action::Pause]);
        assert!(r.is_target(&song));
        r.written(t + Duration::from_secs(227), Written::Saved { file: "a.flac".into(), note: None });
        let done = r.take_done();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].1, Outcome::Recorded { file: "a.flac".into(), note: None });
        assert!(!r.ready(t + Duration::from_secs(227)), "a gap of silence before the next song");
        assert!(r.ready(t + Duration::from_secs(229)));
    }

    #[test]
    fn corrects_an_album_that_starts_one_track_off() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, false);
        let before = track("Intro", "NYCO", 60_000);
        assert_eq!(r.tick(t + Duration::from_secs(1), &playing(&before, 100)), vec![Action::Next]);
        // Still showing the wrong song for a moment: no new action.
        assert!(r.tick(t + Duration::from_millis(1_500), &playing(&before, 600)).is_empty());
        let right = track("Feel It", "NYCO", 225_000);
        assert!(r.tick(t + Duration::from_secs(2), &playing(&right, 50)).is_empty());
        r.event(t + Duration::from_secs(2), &Event::Started(right));
        assert!(r.recording());
    }

    #[test]
    fn gives_up_after_next_and_two_previous() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, false);
        let songs = ["A", "B", "C", "D"].map(|s| track(s, "X", 100_000));
        assert_eq!(r.tick(t, &playing(&songs[0], 0)), vec![Action::Next]);
        assert_eq!(r.tick(t, &playing(&songs[1], 0)), vec![Action::Previous]);
        assert_eq!(r.tick(t, &playing(&songs[2], 0)), vec![Action::Previous]);
        assert_eq!(r.tick(t, &playing(&songs[3], 0)), vec![Action::Pause]);
        assert!(matches!(r.take_done()[0].1, Outcome::WrongSong { .. }));
    }

    #[test]
    fn user_actions_are_a_take_over_and_leave_the_player_alone() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, false);
        let song = track("Feel It", "NYCO", 225_000);
        r.event(t, &Event::Started(song.clone()));
        let actions = r.event(t + Duration::from_secs(30), &Event::Dropped { track: song, reason: Reason::Paused });
        assert!(actions.is_empty());
        assert!(matches!(r.take_done()[0].1, Outcome::TookOver { .. }));
    }

    #[test]
    fn a_spoilt_start_is_tried_once_more() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, false);
        let song = track("Feel It", "NYCO", 225_000);
        r.event(t, &Event::Started(song.clone()));
        // The new audio stream came up at the wrong volume: the engine fixed it and dropped the recording.
        assert_eq!(r.event(t + Duration::from_millis(400), &Event::Dropped { track: song.clone(), reason: Reason::VolumeChanged }), vec![Action::Pause]);
        assert!(r.take_done().is_empty());
        assert!(r.tick(t + Duration::from_millis(500), &Media::default()).is_empty());
        assert_eq!(r.tick(t + Duration::from_secs(2), &Media::default()), vec![Action::Open]);
        // The second time it counts.
        r.event(t + Duration::from_secs(3), &Event::Started(song.clone()));
        r.event(t + Duration::from_secs(3), &Event::Dropped { track: song, reason: Reason::VolumeChanged });
        assert!(matches!(r.take_done()[0].1, Outcome::TookOver { .. }));
    }

    #[test]
    fn a_pause_right_after_the_start_starts_the_song_again() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, false);
        let song = track("Feel It", "NYCO", 225_000);
        r.event(t, &Event::Started(song.clone()));
        assert_eq!(r.event(t + Duration::from_secs(1), &Event::Dropped { track: song, reason: Reason::Paused }), vec![Action::Pause]);
        assert!(r.take_done().is_empty());
        assert_eq!(r.tick(t + Duration::from_secs(3), &Media::default()), vec![Action::Open]);
    }

    #[test]
    fn waits_while_the_user_plays_something() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        assert!(r.assign(job(), t, true).is_empty());
        let other = playing(&track("Mine", "Me", 100_000), 5_000);
        assert!(r.tick(t + Duration::from_secs(5), &other).is_empty());
        assert_eq!(r.tick(t + Duration::from_secs(6), &Media::default()), vec![Action::Open]);
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, true);
        assert!(r.tick(t + Duration::from_secs(11), &other).is_empty());
        assert!(matches!(r.take_done()[0].1, Outcome::TookOver { .. }));
    }

    #[test]
    fn no_sound_on_this_pc() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, false);
        let song = track("Feel It", "NYCO", 225_000);
        r.tick(t, &playing(&song, 0));
        assert!(r.tick(t + Duration::from_secs(3), &playing(&song, 3_000)).is_empty());
        assert_eq!(r.tick(t + Duration::from_secs(7), &playing(&song, 7_000)), vec![Action::Pause]);
        assert_eq!(r.take_done()[0].1, Outcome::NoSound);
    }

    #[test]
    fn a_player_that_never_starts_fails() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, false);
        assert!(r.tick(t + Duration::from_secs(20), &Media::default()).is_empty(), "the first start may take long");
        assert_eq!(r.tick(t + Duration::from_secs(26), &Media::default()), vec![Action::Pause]);
        assert!(matches!(r.take_done()[0].1, Outcome::Failed { .. }));
    }

    #[test]
    fn other_tracks_completing_are_not_the_target() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, false);
        let other = track("Next One", "NYCO", 200_000);
        assert!(r.event(t, &Event::Completed { track: other.clone(), samples: vec![], off_grid: 0, off_span: None }).is_empty());
        assert!(!r.is_target(&other));
    }
}

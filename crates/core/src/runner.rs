//! Plays queued songs one after another and records each: open the link, check the right song plays, let
//! [`crate::passive`] record it, pause at the exact end (or the album plays on), and leave a gap of silence before
//! the next one so every song gets a clean start. Anything the runner didn't cause means the user took over.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::passive::{Event, Media, Reason, Track};

const GAP: Duration = Duration::from_millis(1_200);
const START_TIMEOUT: Duration = Duration::from_secs(12);
/// The player may still be launching.
const FIRST_START_TIMEOUT: Duration = Duration::from_secs(25);
const CORRECTION_TIMEOUT: Duration = Duration::from_secs(4);
/// The right song plays but no recording starts: no sound reaches this PC (Spotify Connect).
const SOUND_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_START_POSITION_MS: i64 = 3_000;
/// Play once the seek to 0:00 shows, or after `SEEK_WAIT`: seek and play at once resumed where it was for a moment.
const SEEKED_MS: i64 = 1_000;
const SEEK_WAIT: Duration = Duration::from_secs(1);
/// A new song may briefly show the previous position.
const POSITION_SETTLE: Duration = Duration::from_millis(1_500);
const LENGTH_TOLERANCE_MS: u64 = 2_000;
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);
/// An album may start one track off: next, then back past the original to the previous one.
const CORRECTIONS: [Action; 3] = [Action::Next, Action::Previous, Action::Previous];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Job {
    /// Echolot's item id (`None` for an exported list).
    pub id: Option<u64>,
    /// `spotify:track:<id>`
    pub uri: String,
    pub artist: String,
    pub artists: Vec<String>,
    pub title: String,
    pub album: String,
    pub duration_ms: u64,
    pub isrc: String,
}

impl Job {
    pub fn track_id(&self) -> &str {
        self.uri.rsplit(':').next().unwrap_or_default()
    }

    pub fn play_uri(&self) -> String {
        format!("{}:play", self.uri.trim_end_matches(":play"))
    }

    pub fn label(&self) -> String {
        format!("{} - {}", self.artist, self.title)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open the job's link (starts the player if needed).
    Open,
    Play,
    Pause,
    Next,
    Previous,
    /// Back to 0:00.
    Restart,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// `note` explains anything not bit-exact (the player's fade-in).
    Recorded { file: PathBuf, note: Option<String> },
    LossyOnly { detail: String },
    WrongSong { detail: String },
    TookOver { detail: String },
    /// Buffering, a glitch, no clean start: worth another try.
    Stalled { detail: String },
    Failed { detail: String },
    /// The right song plays but no sound reaches this PC.
    NoSound,
}

/// What became of a finished recording, reported by its writer.
#[derive(Debug, Clone, PartialEq)]
pub enum Written {
    Saved { file: PathBuf, note: Option<String> },
    Lossy { detail: String },
    /// Damaged: never uploaded, worth another try.
    Corrupt { detail: String },
    Failed { error: String },
}

#[derive(Debug)]
enum Stage {
    Idle { since: Instant },
    /// The user plays something: wait before starting.
    Waiting { job: Job, since: Instant },
    /// Waiting for the player to show the song. `late`: since when it shows a position beyond the start.
    Starting { job: Job, at: Instant, timeout: Duration, corrections: usize, wrong: Option<Track>, retried: bool, rewound: bool, late: Option<Instant> },
    Playing { job: Job, at: Instant, retried: bool },
    Recording { job: Job, at: Instant, retried: bool },
    Writing { job: Job },
    /// Back to 0:00 after the gap, then play: opening the link again would only resume the song.
    Rewind { job: Job, since: Instant, retried: bool, seeked: bool },
}

pub struct Runner {
    stage: Stage,
    first: bool,
    done: Vec<(Job, Outcome)>,
}

impl Runner {
    pub fn new(now: Instant) -> Runner {
        Runner { stage: Stage::Idle { since: now }, first: true, done: Vec::new() }
    }

    pub fn ready(&self, now: Instant) -> bool {
        matches!(self.stage, Stage::Idle { since } if now >= since + GAP)
    }

    pub fn job(&self) -> Option<&Job> {
        match &self.stage {
            Stage::Idle { .. } => None,
            Stage::Waiting { job, .. } | Stage::Starting { job, .. } | Stage::Playing { job, .. } | Stage::Recording { job, .. } | Stage::Writing { job } | Stage::Rewind { job, .. } => Some(job),
        }
    }

    pub fn recording(&self) -> bool {
        matches!(self.stage, Stage::Recording { .. })
    }

    pub fn writing(&self) -> bool {
        matches!(self.stage, Stage::Writing { .. })
    }

    /// `media`: what the player shows now.
    pub fn assign(&mut self, job: Job, now: Instant, media: &Media) -> Vec<Action> {
        if media.playing {
            self.stage = Stage::Waiting { job, since: now };
            return Vec::new();
        }
        self.open(job, now, media)
    }

    fn open(&mut self, job: Job, now: Instant, media: &Media) -> Vec<Action> {
        let timeout = if self.first { FIRST_START_TIMEOUT } else { START_TIMEOUT };
        self.first = false;
        // Already loaded: its link would resume it, faded in over 10 ms. From 0:00 with Play it starts bit-exact.
        if media.track.as_ref().is_some_and(|t| matches(&job, t).is_ok()) {
            self.stage = Stage::Rewind { job, since: now, retried: false, seeked: true };
            return vec![Action::Restart];
        }
        self.stage = Stage::Starting { job, at: now, timeout, corrections: 0, wrong: None, retried: false, rewound: false, late: None };
        vec![Action::Open]
    }

    pub fn take_done(&mut self) -> Vec<(Job, Outcome)> {
        std::mem::take(&mut self.done)
    }

    /// Gives up the current job (the engine stops); a song being written is left to its writer.
    pub fn abort(&mut self, now: Instant, outcome: Outcome) -> Vec<Action> {
        match self.job().cloned() {
            Some(job) if !self.writing() => self.finish(job, outcome, now, true),
            _ => Vec::new(),
        }
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
                    return self.open(job, now, media);
                }
                if now.duration_since(*since) < BUSY_TIMEOUT {
                    return Vec::new();
                }
                self.finish(job, Outcome::TookOver { detail: "Spotify is in use".into() }, now, false)
            }
            Stage::Rewind { job, since, retried, seeked } => {
                if !*seeked {
                    if now < *since + GAP {
                        return Vec::new();
                    }
                    (*since, *seeked) = (now, true);
                    return vec![Action::Restart];
                }
                if media.position_ms > SEEKED_MS && now < *since + SEEK_WAIT {
                    return Vec::new();
                }
                let (job, retried) = (job.clone(), *retried);
                self.stage = Stage::Starting { job, at: now, timeout: START_TIMEOUT, corrections: 0, wrong: None, retried, rewound: true, late: None };
                vec![Action::Play]
            }
            Stage::Playing { job, at, retried } => {
                // Paused before the recording began (a key reached the player while it had the focus): once more.
                if !media.playing && media.track.is_some() {
                    let (job, retried) = (job.clone(), *retried);
                    if !retried {
                        self.stage = Stage::Rewind { job, since: now, retried: true, seeked: false };
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
            Stage::Starting { job, at, timeout, corrections, wrong, retried, rewound, late } => {
                let showing = media.track.as_ref().filter(|_| media.playing);
                let Some(track) = showing.filter(|t| wrong.as_ref() != Some(*t)) else {
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
                    // Beyond the start for longer than a stale position lasts: the player resumed the song somewhere.
                    // The right song is put back to the start, once.
                    if now.duration_since(*late.get_or_insert(now)) < POSITION_SETTLE {
                        return Vec::new();
                    }
                    if matches(job, track).is_ok() && !*rewound {
                        let (job, retried) = (job.clone(), *retried);
                        self.stage = Stage::Rewind { job, since: now, retried, seeked: false };
                        return vec![Action::Pause];
                    }
                    if now.duration_since(*at) < *timeout {
                        return Vec::new();
                    }
                    let job = job.clone();
                    return self.finish(job, Outcome::Failed { detail: format!("the song started at {} s instead of 0:00", media.position_ms / 1000) }, now, true);
                }
                *late = None;
                if matches(job, track).is_ok() {
                    let (job, retried) = (job.clone(), *retried);
                    self.stage = Stage::Playing { job, at: now, retried };
                    return Vec::new();
                }
                let (job, track) = (job.clone(), track.clone());
                self.correct(job, track, now)
            }
        }
    }

    fn correct(&mut self, job: Job, wrong: Track, now: Instant) -> Vec<Action> {
        let Stage::Starting { corrections, retried, rewound, .. } = self.stage else { return Vec::new() };
        match CORRECTIONS.get(corrections) {
            Some(&action) => {
                self.stage = Stage::Starting { job, at: now, timeout: START_TIMEOUT, corrections: corrections + 1, wrong: Some(wrong), retried, rewound, late: None };
                vec![action]
            }
            None => {
                let detail = format!("\"{} - {}\" played instead, also after going back and forward", wrong.artist, wrong.title);
                self.finish(job, Outcome::WrongSong { detail }, now, true)
            }
        }
    }

    pub fn event(&mut self, now: Instant, event: &Event) -> Vec<Action> {
        match event {
            Event::Started(track) => {
                if let Stage::Starting { job, retried, .. } | Stage::Playing { job, retried, .. } = &self.stage
                    && matches(job, track).is_ok()
                {
                    self.stage = Stage::Recording { job: job.clone(), at: now, retried: *retried };
                }
                Vec::new()
            }
            Event::Dropped { track, reason } => {
                let (job, at, retried) = match &self.stage {
                    Stage::Recording { job, at, retried } if matches(job, track).is_ok() => (job.clone(), *at, *retried),
                    // Before the recording: only a start that can never be clean ends the job.
                    Stage::Playing { job, at, retried } if matches(job, track).is_ok() && matches!(reason, Reason::MidTrack | Reason::NoCleanStart) => (job.clone(), *at, *retried),
                    _ => return Vec::new(),
                };
                // Spoilt by the start itself (a stream that came up at the wrong volume, a key that reached the player
                // while it had the focus): once more from silence.
                let startup = matches!(reason, Reason::VolumeChanged | Reason::NoCleanStart | Reason::MidTrack | Reason::Paused) && now.duration_since(at) < Duration::from_secs(4);
                if startup && !retried {
                    self.stage = Stage::Rewind { job, since: now, retried: true, seeked: false };
                    return vec![Action::Pause];
                }
                let detail = reason.message().to_owned();
                let outcome = match reason {
                    Reason::Paused | Reason::Seeked | Reason::TrackChanged | Reason::VolumeChanged => Outcome::TookOver { detail },
                    Reason::Underrun | Reason::Glitch | Reason::Stopped | Reason::MidTrack | Reason::NoCleanStart | Reason::DeviceChanged => Outcome::Stalled { detail },
                };
                // After a take-over the player is the user's.
                let pause = !matches!(outcome, Outcome::TookOver { .. });
                self.finish(job, outcome, now, pause)
            }
            Event::Completed { track, .. } => {
                if let Stage::Recording { job, .. } = &self.stage
                    && matches(job, track).is_ok()
                {
                    self.stage = Stage::Writing { job: job.clone() };
                    return vec![Action::Pause];
                }
                Vec::new()
            }
        }
    }

    /// A completed recording belongs to the current job.
    pub fn is_target(&self, track: &Track) -> bool {
        matches!(&self.stage, Stage::Writing { job } if matches(job, track).is_ok())
    }

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
            Written::Corrupt { detail } => Outcome::Stalled { detail },
            Written::Failed { error } => Outcome::Failed { detail: error },
        }
    }
}

/// Lower case letters and digits (umlauts kept), single spaces.
fn norm(s: &str) -> String {
    let s: String = s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The title without "(feat. …)", "[…]", "(… Remix)" or " - Radio Edit".
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

/// The player shows the job's song: the title (additions tolerated), one of the artists, the length within 2 s.
pub fn matches(job: &Job, track: &Track) -> Result<(), String> {
    let (a, b) = (norm(&job.title), norm(&track.title));
    if a != b && (base_title(&job.title).is_empty() || base_title(&job.title) != base_title(&track.title)) {
        return Err(format!("title is \"{}\", expected \"{}\"", track.title, job.title));
    }
    if job.duration_ms > 0 && track.duration_ms.abs_diff(job.duration_ms) > LENGTH_TOLERANCE_MS {
        return Err(format!("length is {} s, expected {} s", track.duration_ms / 1000, job.duration_ms / 1000));
    }
    // The app shows a renamed artist before the Web API does ("TheDoDo" -> "Pbb Yea"): the same title and length is
    // the song, a neighbour after an off-by-one differs in both.
    let renamed = a == b && job.duration_ms > 0;
    let shown = norm(&track.artist);
    let artist_ok = std::iter::once(&job.artist).chain(&job.artists).map(|n| norm(n)).any(|n| !n.is_empty() && (shown.contains(&n) || n.contains(&shown)));
    if !artist_ok && !shown.is_empty() && !renamed {
        return Err(format!("artist is \"{}\", expected \"{}\"", track.artist, job.artist));
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

    fn busy() -> Media {
        playing(&track("Mine", "Me", 100_000), 5_000)
    }

    #[test]
    fn a_song_already_loaded_is_started_from_zero_not_resumed_by_its_link() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        let loaded = Media { playing: false, track: Some(track("Feel It", "NYCO", 225_000)), position_ms: 0 };
        assert_eq!(r.assign(job(), t, &loaded), vec![Action::Restart]);
        // Play only once the player shows the start (or after a moment), never together with the seek.
        let still = Media { position_ms: 95_000, ..loaded.clone() };
        assert!(r.tick(t + Duration::from_millis(300), &still).is_empty());
        assert_eq!(r.tick(t + Duration::from_millis(400), &loaded), vec![Action::Play]);
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, &loaded);
        assert_eq!(r.tick(t + SEEK_WAIT, &still), vec![Action::Play]);
        let mut r = Runner::new(t - GAP);
        let other = Media { playing: false, track: Some(track("Other Song", "NYCO", 200_000)), position_ms: 0 };
        assert_eq!(r.assign(job(), t, &other), vec![Action::Open]);
    }

    #[test]
    fn matching_tolerates_case_feat_and_versions_but_not_length() {
        let j = job();
        assert!(matches(&j, &track("feel it", "NYCO, Someone", 225_900)).is_ok());
        assert!(matches(&j, &track("Feel It (feat. Someone)", "NYCO", 225_000)).is_ok());
        assert!(matches(&j, &track("Feel It - Radio Edit", "NYCO", 224_000)).is_ok());
        assert!(matches(&j, &track("Feel It", "NYCO", 180_000)).is_err(), "another version has another length");
        assert!(matches(&j, &track("Other Song", "NYCO", 225_000)).is_err());
        assert!(matches(&j, &track("Feel It", "Renamed Artist", 225_400)).is_ok(), "a renamed artist: same title and length");
        assert!(matches(&j, &track("Feel It - Radio Edit", "Somebody Else", 225_000)).is_err());
        assert!(matches(&Job { duration_ms: 0, ..job() }, &track("Feel It", "Somebody Else", 225_000)).is_err());
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
        assert_eq!(r.assign(job(), t, &Media::default()), vec![Action::Open]);
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
        r.assign(job(), t, &Media::default());
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
        r.assign(job(), t, &Media::default());
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
        r.assign(job(), t, &Media::default());
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
        r.assign(job(), t, &Media::default());
        let song = track("Feel It", "NYCO", 225_000);
        r.event(t, &Event::Started(song.clone()));
        // The new audio stream came up at the wrong volume: the engine fixed it and dropped the recording.
        assert_eq!(r.event(t + Duration::from_millis(400), &Event::Dropped { track: song.clone(), reason: Reason::VolumeChanged }), vec![Action::Pause]);
        assert!(r.take_done().is_empty());
        assert!(r.tick(t + Duration::from_millis(500), &Media::default()).is_empty());
        assert_eq!(r.tick(t + Duration::from_secs(2), &Media::default()), vec![Action::Restart]);
        assert_eq!(r.tick(t + Duration::from_millis(2_100), &Media::default()), vec![Action::Play]);
        // The second time it counts.
        r.event(t + Duration::from_secs(3), &Event::Started(song.clone()));
        r.event(t + Duration::from_secs(3), &Event::Dropped { track: song, reason: Reason::VolumeChanged });
        assert!(matches!(r.take_done()[0].1, Outcome::TookOver { .. }));
    }

    #[test]
    fn a_pause_right_after_the_start_starts_the_song_again() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, &Media::default());
        let song = track("Feel It", "NYCO", 225_000);
        r.event(t, &Event::Started(song.clone()));
        assert_eq!(r.event(t + Duration::from_secs(1), &Event::Dropped { track: song, reason: Reason::Paused }), vec![Action::Pause]);
        assert!(r.take_done().is_empty());
        assert_eq!(r.tick(t + Duration::from_secs(3), &Media::default()), vec![Action::Restart]);
        assert_eq!(r.tick(t + Duration::from_millis(3_100), &Media::default()), vec![Action::Play]);
    }

    #[test]
    fn waits_while_the_user_plays_something() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        assert!(r.assign(job(), t, &busy()).is_empty());
        let other = playing(&track("Mine", "Me", 100_000), 5_000);
        assert!(r.tick(t + Duration::from_secs(5), &other).is_empty());
        assert_eq!(r.tick(t + Duration::from_secs(6), &Media::default()), vec![Action::Open]);
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, &busy());
        assert!(r.tick(t + Duration::from_secs(11), &other).is_empty());
        assert!(matches!(r.take_done()[0].1, Outcome::TookOver { .. }));
    }

    #[test]
    fn a_song_that_starts_beyond_the_beginning_is_put_back_to_the_start_once() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, &Media::default());
        let song = track("Feel It", "NYCO", 225_000);
        // A stale position for a moment is no reason to act.
        assert!(r.tick(t + Duration::from_millis(300), &playing(&song, 95_000)).is_empty());
        assert!(r.tick(t + Duration::from_millis(1_000), &playing(&song, 95_700)).is_empty());
        // It stays beyond the start: the player resumed the song. Pause, then from the beginning after the gap.
        assert_eq!(r.tick(t + Duration::from_millis(2_000), &playing(&song, 96_700)), vec![Action::Pause]);
        let paused = Media { playing: false, track: Some(song.clone()), position_ms: 96_700 };
        assert!(r.tick(t + Duration::from_millis(2_500), &paused).is_empty());
        assert_eq!(r.tick(t + Duration::from_millis(3_300), &paused), vec![Action::Restart]);
        let at_start = Media { position_ms: 0, ..paused.clone() };
        assert_eq!(r.tick(t + Duration::from_millis(3_400), &at_start), vec![Action::Play]);
        assert!(r.tick(t + Duration::from_millis(3_600), &playing(&song, 200)).is_empty());
        r.event(t + Duration::from_millis(3_600), &Event::Started(song.clone()));
        assert!(r.recording());
        // Only once: a song that still won't start at the beginning fails.
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, &Media::default());
        r.tick(t, &playing(&song, 95_000));
        r.tick(t + Duration::from_secs(2), &playing(&song, 97_000));
        r.tick(t + Duration::from_secs(4), &paused);
        r.tick(t + Duration::from_secs(5), &playing(&song, 99_000));
        r.tick(t + Duration::from_secs(7), &playing(&song, 101_000));
        assert!(r.tick(t + Duration::from_secs(18), &playing(&song, 112_000)).contains(&Action::Pause));
        assert!(matches!(&r.take_done()[0].1, Outcome::Failed { detail } if detail.contains("instead of 0:00")));
    }

    #[test]
    fn no_sound_on_this_pc() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, &Media::default());
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
        r.assign(job(), t, &Media::default());
        assert!(r.tick(t + Duration::from_secs(20), &Media::default()).is_empty(), "the first start may take long");
        assert_eq!(r.tick(t + Duration::from_secs(26), &Media::default()), vec![Action::Pause]);
        assert!(matches!(r.take_done()[0].1, Outcome::Failed { .. }));
    }

    #[test]
    fn other_tracks_completing_are_not_the_target() {
        let t = Instant::now();
        let mut r = Runner::new(t - GAP);
        r.assign(job(), t, &Media::default());
        let other = track("Next One", "NYCO", 200_000);
        assert!(r.event(t, &Event::Completed { track: other.clone(), samples: vec![], off_grid: 0, off_span: None }).is_empty());
        assert!(!r.is_target(&other));
    }
}

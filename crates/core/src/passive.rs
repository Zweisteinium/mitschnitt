//! Passive mode: record every track that plays from its very first sample to its exact end, and
//! discard anything that isn't provably complete.
//!
//! The state machine is fed with captured packets ([`Passive::push`]) and media snapshots
//! ([`Passive::media`]) and knows nothing about Windows, so it is fully unit-testable.
//!
//! Start: a track is only recorded if it begins after at least [`MIN_SILENCE_MS`] of digital silence
//! (play pressed, or the previous track ended in silence) - that silence-to-audio edge is the exact first
//! sample. If the player's stream itself starts there (packets flagged "no data" before), the start is the
//! first rendered frame instead, so a master's leading digital silence is kept. Metadata may arrive before
//! or after the audio, so recent clean starts are remembered for [`LOOKBACK_MS`] and matched against the
//! player's position.
//! End: exactly the track length the player reports (millisecond precision).
//! Discard: pause, seek, skip, a stalled player (silent packets), capture glitches, volume changes.
//! Source proof: a lossless source played untouched reaches us as exact multiples of 2^-24 (24-bit grid at
//! gain 0.5); decoded lossy audio or any processing (normalization, EQ, resampling) leaves that grid. Every
//! sample is checked before it is rounded, per track and live.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`: the capture lost data.
pub const FLAG_GLITCH: u32 = 0x1;
/// `AUDCLNT_BUFFERFLAGS_SILENT`: the player delivered no audio for this packet.
pub const FLAG_SILENT: u32 = 0x2;

/// Spotify leaves exactly 10 ms of digital silence between tracks when the user switches.
const MIN_SILENCE_MS: u64 = 10;
/// Rendered zeros that were already there when the capture started prove nothing (the stream warms up with
/// a few ms of zeros even mid-track); they only count as a start when they lasted this long. A stream that
/// wasn't rendering at all ("silent" packets) is real silence.
const MIN_INITIAL_SILENCE_MS: u64 = 1_000;
/// A recording dropped this early because the title changed was most likely started under stale metadata:
/// its clean start is handed back so the actual track can claim it.
const RELEASE_START_MS: u64 = 3_000;
/// Leading digital silence of a master that is kept when the player's stream starts right before it.
const MAX_LEAD_MS: u64 = 3_000;
const LOOKBACK_MS: u64 = 6_000;
const WAIT_MS: u64 = 5_000;
const START_TOLERANCE_MS: i64 = 3_000;
const POSITION_MATCH_MS: i64 = 1_500;
const SEEK_TOLERANCE_MS: i64 = 3_000;
/// A new title may briefly come with the previous track's position: only a position that stays this long means the
/// track really was joined in the middle.
const POSITION_SETTLE_MS: u64 = 2_000;
/// Players announce the next track shortly before the old one has played out of their buffer.
const EARLY_ANNOUNCE_MS: u64 = 1_500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Media {
    pub playing: bool,
    pub track: Option<Track>,
    /// Current position, extrapolated to "now" while playing.
    pub position_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Paused,
    TrackChanged,
    Seeked,
    Underrun,
    Glitch,
    VolumeChanged,
    Stopped,
    MidTrack,
    NoCleanStart,
    DeviceChanged,
}

impl Reason {
    pub fn message(self) -> &'static str {
        match self {
            Reason::Paused => "Paused",
            Reason::TrackChanged => "Track changed before its end",
            Reason::Seeked => "Seeked within the track",
            Reason::Underrun => "The player stalled (buffering) - dropout",
            Reason::Glitch => "Capture glitch",
            Reason::VolumeChanged => "App volume changed",
            Reason::Stopped => "Stopped",
            Reason::MidTrack => "Joined mid-track",
            Reason::NoCleanStart => "No silence before the track (gapless) - exact start unknown",
            Reason::DeviceChanged => "Audio devices changed during the track",
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Started(Track),
    /// Exactly `duration_ms` worth of frames, restored to the original level (24-bit integers).
    /// `off_grid`: frames that were not exact 24-bit values before rounding (0 = untouched lossless source);
    /// `off_span`: first and last such frame, relative to the track start.
    Completed { track: Track, samples: Vec<i32>, off_grid: usize, off_span: Option<(usize, usize)> },
    Dropped { track: Track, reason: Reason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Waiting,
    Recording,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub track: Option<Track>,
    pub recorded_ms: u64,
    /// Peak per channel since the last status (fraction of full scale, original level).
    pub level: [f32; 2],
    /// RMS per channel since the last status (fraction of full scale).
    pub rms: [f32; 2],
    /// Grid check of the audio since the last status; `None` when only silence arrived.
    pub grid: Option<Grid>,
}

/// How much of the audio is exact: share of non-zero frames on the 24-bit / 16-bit grid (0..1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Grid {
    pub bit24: f32,
    pub bit16: f32,
}

#[derive(Default)]
struct GridCount {
    frames: u64,
    off24: u64,
    off16: u64,
}

impl GridCount {
    fn take(&mut self) -> Option<Grid> {
        let c = std::mem::take(self);
        (c.frames > 0).then(|| Grid { bit24: 1.0 - c.off24 as f32 / c.frames as f32, bit16: 1.0 - c.off16 as f32 / c.frames as f32 })
    }
}

enum State {
    Idle,
    Waiting { track: Track, until: u64 },
    Recording { track: Track, expected: usize, samples: Vec<i32>, off_grid: usize, off_span: Option<(usize, usize)>, start: u64 },
}

pub struct Passive {
    rate: u32,
    channels: usize,
    inv_gain: f64,
    state: State,
    /// Absolute index of the next frame to arrive.
    frame: u64,
    /// The last [`LOOKBACK_MS`] of restored samples: frames `[ring_first, frame)`.
    ring: VecDeque<i32>,
    /// Per ring frame: was it off the 24-bit grid before rounding?
    ring_off: VecDeque<bool>,
    ring_first: u64,
    zero_run: u64,
    /// First frame of the current zero run, and whether it contains "silent" packets (stream not rendering).
    zero_from: u64,
    zero_silent: bool,
    /// Frames of consecutive "no data" packets, and where the player's stream last (re)started rendering.
    silent_run: u64,
    render_start: Option<u64>,
    /// Clean starts (first frame of a track after enough digital silence) still inside the ring.
    starts: VecDeque<u64>,
    /// Last track that was finished or given up on - not retried until it changes or restarts.
    done: Option<Track>,
    /// Position while playing, to notice a restart from the beginning.
    last_position_ms: i64,
    /// A track first shown beyond the start, and the frame it was first seen at (see [`POSITION_SETTLE_MS`]).
    late: Option<(Track, u64)>,
    level: [f32; 2],
    /// Sum of squares per channel and the frames it covers, since the last status.
    energy: ([f64; 2], u64),
    grid: GridCount,
    scratch: Vec<i32>,
}

impl Passive {
    /// `gain` is the exact factor the player applied (app volume), undone on every sample.
    pub fn new(rate: u32, channels: usize, gain: f32) -> Self {
        let ring_frames = ms_to_frames(LOOKBACK_MS, rate);
        Passive {
            rate,
            channels,
            inv_gain: 1.0 / gain as f64,
            state: State::Idle,
            frame: 0,
            ring: VecDeque::with_capacity(ring_frames * channels),
            ring_off: VecDeque::with_capacity(ring_frames),
            ring_first: 0,
            zero_run: 0,
            zero_from: 0,
            zero_silent: false,
            silent_run: 0,
            render_start: None,
            starts: VecDeque::new(),
            done: None,
            last_position_ms: 0,
            late: None,
            level: [0.0; 2],
            energy: ([0.0; 2], 0),
            grid: GridCount::default(),
            scratch: vec![0; channels],
        }
    }

    /// Feed one captured packet (`data` is interleaved; empty or ignored when `FLAG_SILENT` is set).
    pub fn push(&mut self, data: &[f32], frames: usize, flags: u32) -> Vec<Event> {
        let mut events = Vec::new();
        let silent = flags & FLAG_SILENT != 0 || data.len() < frames * self.channels;
        if flags & FLAG_GLITCH != 0 {
            self.drop_recording(Reason::Glitch, &mut events);
        }
        if silent && matches!(self.state, State::Recording { .. }) {
            self.drop_recording(Reason::Underrun, &mut events);
        }

        let ch = self.channels;
        let min_silence = ms_to_frames(MIN_SILENCE_MS, self.rate) as u64;
        let ring_frames = ms_to_frames(LOOKBACK_MS, self.rate) as u64;
        let max_lead = ms_to_frames(MAX_LEAD_MS, self.rate) as u64;
        if silent {
            self.silent_run += frames as u64;
            self.render_start = None;
        } else {
            if self.silent_run >= min_silence {
                self.render_start = Some(self.frame);
            }
            self.silent_run = 0;
        }
        for f in 0..frames {
            let (mut zero, mut off24, mut off16) = (true, false, false);
            for c in 0..ch {
                let (v, exact) = if silent { (0, true) } else { self.restore(data[f * ch + c]) };
                self.scratch[c] = v;
                zero &= v == 0;
                off24 |= !exact;
                off16 |= !exact || v % 256 != 0;
                if c < 2 {
                    let x = v as f32 / 8_388_608.0;
                    self.level[c] = self.level[c].max(x.abs());
                    self.energy.0[c] += (x * x) as f64;
                }
            }

            self.energy.1 += 1;
            if !zero {
                self.grid.frames += 1;
                self.grid.off24 += off24 as u64;
                self.grid.off16 += off16 as u64;
            }
            if zero {
                if self.zero_run == 0 {
                    (self.zero_from, self.zero_silent) = (self.frame, false);
                }
                self.zero_silent |= silent;
                self.zero_run += 1;
            } else {
                let initial = self.zero_from == 0 && !self.zero_silent && self.zero_run < ms_to_frames(MIN_INITIAL_SILENCE_MS, self.rate) as u64;
                if self.zero_run >= min_silence && !initial {
                    // The stream started rendering shortly before: its leading zeros belong to the track.
                    let start = self.render_start.take().filter(|r| self.frame - r <= max_lead).unwrap_or(self.frame);
                    self.starts.push_back(start);
                    if let State::Waiting { track, .. } = &self.state {
                        let track = track.clone();
                        self.start_recording(track, start, &mut events);
                    }
                }
                self.zero_run = 0;
            }

            self.ring.extend(self.scratch.iter().copied());
            self.ring_off.push_back(off24);
            if self.frame + 1 - self.ring_first > ring_frames {
                self.ring.drain(..ch);
                self.ring_off.pop_front();
                self.ring_first += 1;
                while self.starts.front().is_some_and(|s| *s < self.ring_first) {
                    self.starts.pop_front();
                }
            }

            if let State::Recording { expected, samples, off_grid, off_span, .. } = &mut self.state {
                samples.extend_from_slice(&self.scratch);
                if off24 {
                    let i = samples.len() / ch - 1;
                    *off_grid += 1;
                    *off_span = Some(off_span.map_or((i, i), |(a, _)| (a, i)));
                }
                if samples.len() >= *expected * ch {
                    self.complete(&mut events);
                }
            }
            self.frame += 1;
        }

        if let State::Waiting { until, .. } = self.state && self.frame >= until {
            let State::Waiting { track, .. } = std::mem::replace(&mut self.state, State::Idle) else { unreachable!() };
            self.done = Some(track.clone());
            events.push(Event::Dropped { track, reason: Reason::NoCleanStart });
        }
        events
    }

    /// Feed a media snapshot (poll it a few times per second).
    pub fn media(&mut self, m: Media) -> Vec<Event> {
        let mut events = Vec::new();
        match &self.state {
            State::Recording { track, expected, samples, .. } => {
                let recorded = samples.len() / self.channels;
                let remaining_ms = frames_to_ms(expected.saturating_sub(recorded), self.rate);
                let elapsed_ms = frames_to_ms(recorded, self.rate) as i64;
                if !m.playing {
                    self.drop_recording(Reason::Paused, &mut events);
                } else if m.track.as_ref() != Some(track) {
                    if remaining_ms > EARLY_ANNOUNCE_MS {
                        self.drop_recording(Reason::TrackChanged, &mut events);
                    }
                } else if (m.position_ms - elapsed_ms).abs() > SEEK_TOLERANCE_MS {
                    self.drop_recording(Reason::Seeked, &mut events);
                }
            }
            State::Waiting { track, .. } => {
                if !m.playing || m.track.as_ref() != Some(track) {
                    self.state = State::Idle;
                }
            }
            State::Idle => {}
        }

        if let (State::Idle, true, Some(track)) = (&self.state, m.playing, &m.track) {
            // Back to the beginning (seeked to 0, or started again): a finished or dropped track may be taken again.
            // The margin only absorbs the jitter of the player's reported position.
            let restarted = m.position_ms <= START_TOLERANCE_MS && self.last_position_ms > m.position_ms + 250;
            if restarted || self.done.as_ref() != Some(track) {
                self.consider_start(track.clone(), m.position_ms, &mut events);
            }
        }
        if m.playing {
            self.last_position_ms = m.position_ms;
        }
        events
    }

    /// Something outside the stream spoilt the recording (e.g. audio devices changed): drop it.
    pub fn interrupt(&mut self, reason: Reason) -> Vec<Event> {
        let mut events = Vec::new();
        self.drop_recording(reason, &mut events);
        events
    }

    /// The app's volume left the value the recording relies on.
    pub fn volume_changed(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        self.drop_recording(Reason::VolumeChanged, &mut events);
        events
    }

    pub fn stop(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        self.drop_recording(Reason::Stopped, &mut events);
        self.state = State::Idle;
        events
    }

    pub fn recording(&self) -> bool {
        matches!(self.state, State::Recording { .. })
    }

    /// How much of the current track is recorded so far.
    pub fn recorded_ms(&self) -> u64 {
        match &self.state {
            State::Recording { samples, .. } => frames_to_ms(samples.len() / self.channels, self.rate),
            _ => 0,
        }
    }

    /// Current state; resets the level meter.
    pub fn status(&mut self) -> Status {
        let (phase, track, recorded) = match &self.state {
            State::Idle => (Phase::Idle, None, 0),
            State::Waiting { track, .. } => (Phase::Waiting, Some(track.clone()), 0),
            State::Recording { track, samples, .. } => (Phase::Recording, Some(track.clone()), samples.len() / self.channels),
        };
        let level = std::mem::take(&mut self.level);
        let (sums, frames) = std::mem::take(&mut self.energy);
        let rms = sums.map(|s| if frames > 0 { (s / frames as f64).sqrt() as f32 } else { 0.0 });
        Status { phase, track, recorded_ms: frames_to_ms(recorded, self.rate), level, rms, grid: self.grid.take() }
    }

    fn consider_start(&mut self, track: Track, position_ms: i64, events: &mut Vec<Event>) {
        if track.duration_ms == 0 {
            return;
        }
        if position_ms > START_TOLERANCE_MS {
            let first = match &self.late {
                Some((t, frame)) if *t == track => *frame,
                _ => {
                    self.late = Some((track.clone(), self.frame));
                    self.frame
                }
            };
            if self.frame - first < ms_to_frames(POSITION_SETTLE_MS, self.rate) as u64 {
                return;
            }
            self.late = None;
            self.done = Some(track.clone());
            events.push(Event::Dropped { track, reason: Reason::MidTrack });
            return;
        }
        self.late = None;
        // The audio may already be running: the clean start must lie `position` ago (within tolerance).
        let target = self.frame as i64 - ms_to_frames(position_ms.max(0) as u64, self.rate) as i64;
        let tolerance = ms_to_frames(POSITION_MATCH_MS as u64, self.rate) as i64;
        if let Some(&start) = self.starts.iter().rev().find(|s| (**s as i64 - target).abs() <= tolerance) {
            self.start_recording(track, start, events);
        } else {
            let until = self.frame + ms_to_frames(WAIT_MS, self.rate) as u64;
            self.state = State::Waiting { track, until };
        }
    }

    fn start_recording(&mut self, track: Track, start: u64, events: &mut Vec<Event>) {
        let expected = ms_to_frames(track.duration_ms, self.rate);
        let mut samples = Vec::with_capacity(expected * self.channels);
        let first = (start - self.ring_first) as usize;
        samples.extend(self.ring.range(first * self.channels..));
        let off: Vec<usize> = self.ring_off.range(first..).enumerate().filter(|(_, o)| **o).map(|(i, _)| i).collect();
        let (off_grid, off_span) = (off.len(), off.first().map(|a| (*a, *off.last().unwrap_or(a))));
        // A start belongs to exactly one track: never let a later track claim it (or an earlier one).
        self.starts.retain(|s| *s > start);
        events.push(Event::Started(track.clone()));
        self.state = State::Recording { track, expected, samples, off_grid, off_span, start };
        if let State::Recording { expected, samples, .. } = &self.state && samples.len() >= expected * self.channels {
            self.complete(events);
        }
    }

    fn complete(&mut self, events: &mut Vec<Event>) {
        if let State::Recording { track, expected, mut samples, off_grid, off_span, .. } = std::mem::replace(&mut self.state, State::Idle) {
            samples.truncate(expected * self.channels);
            self.done = Some(track.clone());
            events.push(Event::Completed { track, samples, off_grid, off_span });
        }
    }

    fn drop_recording(&mut self, reason: Reason, events: &mut Vec<Event>) {
        if matches!(self.state, State::Recording { .. })
            && let State::Recording { track, samples, start, .. } = std::mem::replace(&mut self.state, State::Idle)
        {
            let early = frames_to_ms(samples.len() / self.channels, self.rate) < RELEASE_START_MS;
            if reason == Reason::TrackChanged && early && start >= self.ring_first {
                let at = self.starts.partition_point(|s| *s < start);
                self.starts.insert(at, start);
            }
            self.done = Some(track.clone());
            events.push(Event::Dropped { track, reason });
        }
    }

    /// Undo the player's gain and quantise to 24 bit; also reports whether the value already was an exact
    /// 24-bit sample (exact for a 0.5 gain on untouched 24/16-bit sources).
    fn restore(&self, x: f32) -> (i32, bool) {
        let v = x as f64 * self.inv_gain * 8_388_608.0;
        let r = v.round();
        (r.clamp(-8_388_608.0, 8_388_607.0) as i32, r == v)
    }
}

fn ms_to_frames(ms: u64, rate: u32) -> usize {
    ((ms * rate as u64 + 500) / 1000) as usize
}

fn frames_to_ms(frames: usize, rate: u32) -> u64 {
    frames as u64 * 1000 / rate as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 8_000;
    const GAIN: f32 = 0.5;

    fn track(ms: u64) -> Track {
        Track { title: "Song".into(), artist: "Artist".into(), album: String::new(), duration_ms: ms }
    }

    /// A deterministic 24-bit master, never exactly zero, and what the player hands over at `GAIN`.
    fn master(frames: usize, seed: i32) -> (Vec<i32>, Vec<f32>) {
        let samples: Vec<i32> = (0..frames * 2).map(|i| ((i as i32 * 7919 + seed) % 4_000_000) + 1_000).collect();
        let raw = samples.iter().map(|s| (*s as f64 / 8_388_608.0 * GAIN as f64) as f32).collect();
        (samples, raw)
    }

    fn feed(p: &mut Passive, raw: &[f32], events: &mut Vec<Event>) {
        for chunk in raw.chunks(160) {
            events.extend(p.push(chunk, chunk.len() / 2, 0));
        }
    }

    fn silence(p: &mut Passive, ms: u64, events: &mut Vec<Event>) {
        let frames = ms_to_frames(ms, RATE);
        events.extend(p.push(&[], frames, FLAG_SILENT));
    }

    fn playing(t: &Track, position_ms: i64) -> Media {
        Media { playing: true, track: Some(t.clone()), position_ms }
    }

    #[test]
    fn records_exactly_from_the_first_sample_to_the_track_length() {
        let t = track(2_000);
        let (orig, raw) = master(ms_to_frames(2_500, RATE), 1); // the player keeps going past the end
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = Vec::new();
        silence(&mut p, 500, &mut ev);
        feed(&mut p, &raw[..800], &mut ev); // audio starts before the metadata arrives ...
        ev.extend(p.media(playing(&t, 50))); // ... which reports ~50 ms in
        feed(&mut p, &raw[800..], &mut ev);
        let done = ev.iter().find_map(|e| match e { Event::Completed { samples, .. } => Some(samples), _ => None }).expect("completed");
        assert_eq!(done.len(), ms_to_frames(2_000, RATE) * 2);
        assert_eq!(done[..], orig[..done.len()], "restored samples must equal the master exactly");
        assert!(ev.iter().any(|e| matches!(e, Event::Completed { off_grid: 0, .. })), "an untouched source is on the grid");
    }

    #[test]
    fn processed_audio_is_off_the_grid() {
        let t = track(1_000);
        let (_, mut raw) = master(ms_to_frames(1_000, RATE), 8);
        for x in &mut raw[1_000..1_100] {
            *x *= 0.97; // e.g. a fade or normalization: no longer exact 24-bit values
        }
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 0));
        silence(&mut p, 100, &mut ev);
        feed(&mut p, &raw, &mut ev);
        let off = ev.iter().find_map(|e| match e { Event::Completed { off_grid, .. } => Some(*off_grid), _ => None }).expect("completed");
        assert!((40..=50).contains(&off), "{off} frames off the grid");
        let grid = p.status().grid.expect("audio arrived");
        assert!(grid.bit24 < 1.0 && grid.bit24 > 0.9, "{grid:?}");
    }

    #[test]
    fn metadata_before_audio_waits_for_the_clean_start() {
        let t = track(1_000);
        let (orig, raw) = master(ms_to_frames(1_000, RATE), 2);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 0));
        assert_eq!(p.status().phase, Phase::Waiting);
        silence(&mut p, 200, &mut ev);
        feed(&mut p, &raw, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, Event::Completed { samples, .. } if samples[..] == orig[..])));
    }

    #[test]
    fn leading_silence_of_the_master_is_kept_when_the_stream_starts() {
        let t = track(1_000);
        let lead = ms_to_frames(20, RATE); // the master begins with 20 ms of digital silence
        let (body, raw_body) = master(ms_to_frames(1_000, RATE) - lead, 7);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 0));
        silence(&mut p, 300, &mut ev); // stream not running: "no data" packets
        ev.extend(p.push(&vec![0.0; lead * 2], lead, 0)); // stream starts: rendered zeros ...
        feed(&mut p, &raw_body, &mut ev); // ... then the music
        let done = ev.iter().find_map(|e| match e { Event::Completed { samples, .. } => Some(samples), _ => None }).expect("completed");
        assert!(done[..lead * 2].iter().all(|s| *s == 0), "leading silence kept");
        assert_eq!(done[lead * 2..], body[..], "then the master, exactly");
    }

    #[test]
    fn stale_title_hands_the_start_to_the_actual_track() {
        // The player reports the new length before the new title: the recording starts under the old name.
        let stale = Track { title: "Previous".into(), ..track(5_000) };
        let actual = track(5_000);
        let (orig, raw) = master(ms_to_frames(5_000, RATE), 9);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = Vec::new();
        silence(&mut p, 2_000, &mut ev);
        ev.extend(p.media(playing(&stale, 0)));
        feed(&mut p, &raw[..800], &mut ev);
        ev.extend(p.media(playing(&actual, 50))); // the title catches up
        feed(&mut p, &raw[800..], &mut ev);
        assert!(ev.iter().any(|e| matches!(e, Event::Dropped { track, reason: Reason::TrackChanged } if track.title == "Previous")));
        let done = ev.iter().find_map(|e| match e { Event::Completed { track, samples, .. } if track.title == "Song" => Some(samples), _ => None });
        assert_eq!(done.expect("actual track recorded")[..], orig[..]);
    }

    #[test]
    fn short_silence_at_capture_start_is_not_a_track_start() {
        // Listening starts 2 s into a song: the stream warms up with a few ms of zeros, then the song goes on.
        let t = track(10_000);
        let (_, raw) = master(ms_to_frames(3_000, RATE), 10);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.push(&vec![0.0; 2 * ms_to_frames(30, RATE)], ms_to_frames(30, RATE), 0);
        feed(&mut p, &raw[..1_600], &mut ev);
        ev.extend(p.media(playing(&t, 2_000)));
        feed(&mut p, &raw[1_600..], &mut ev);
        assert!(!ev.iter().any(|e| matches!(e, Event::Started(_))), "{ev:?}");
    }

    #[test]
    fn a_fade_in_is_located() {
        let t = track(1_000);
        let (_, mut raw) = master(ms_to_frames(1_000, RATE), 11);
        for (i, x) in raw[..160].iter_mut().enumerate() {
            *x *= (i / 2) as f32 / 80.0 + 0.003; // 10 ms ramp at 8 kHz
        }
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 0));
        silence(&mut p, 100, &mut ev);
        feed(&mut p, &raw, &mut ev);
        let span = ev.iter().find_map(|e| match e { Event::Completed { off_span, .. } => Some(*off_span), _ => None }).expect("completed");
        assert!(matches!(span, Some((0, last)) if last < 80), "{span:?}");
    }

    /// Replays a real `listen --trace` folder (raw capture + player timeline) through the state machine:
    /// `MITSCHNITT_TRACE=<dir> cargo test -p mitschnitt-core replay_trace -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn replay_trace() {
        let dir = std::path::PathBuf::from(std::env::var("MITSCHNITT_TRACE").expect("MITSCHNITT_TRACE"));
        let bytes = std::fs::read(dir.join("raw.f32")).unwrap();
        let raw: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        // media lines: frame, "media", playing, "artist - title", duration_ms, position_ms
        let mut media: Vec<(u64, Media)> = Vec::new();
        for line in std::fs::read_to_string(dir.join("events.tsv")).unwrap().lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.get(1) == Some(&"media") {
                let duration_ms: u64 = f[4].parse().unwrap();
                let track = (f[3] != "-").then(|| Track { title: f[3].into(), artist: String::new(), album: String::new(), duration_ms });
                media.push((f[0].parse().unwrap(), Media { playing: f[2] == "true", track, position_ms: f[5].parse().unwrap() }));
            }
        }
        let rate = 44_100;
        let mut p = Passive::new(rate, 2, 0.5);
        let (mut frame, mut next) = (0u64, 0usize);
        for chunk in raw.chunks(441 * 2) {
            for e in p.push(chunk, chunk.len() / 2, 0) {
                report(frame, e);
            }
            frame += (chunk.len() / 2) as u64;
            while next + 1 < media.len() && media[next + 1].0 <= frame {
                next += 1;
            }
            if let Some((at, m)) = media.get(next).filter(|(at, _)| *at <= frame) {
                let mut m = m.clone();
                if m.playing {
                    m.position_ms += ((frame - at) * 1000 / rate as u64) as i64;
                }
                for e in p.media(m) {
                    report(frame, e);
                }
            }
        }
        fn report(frame: u64, e: Event) {
            match e {
                Event::Started(t) => println!("{frame:>9} started   {}", t.title),
                Event::Dropped { track, reason } => println!("{frame:>9} dropped   {} ({reason:?})", track.title),
                Event::Completed { track, off_grid, off_span, .. } => println!("{frame:>9} completed {} off_grid={off_grid} {off_span:?}", track.title),
            }
        }
    }

    #[test]
    fn pause_discards_the_track() {
        let t = track(2_000);
        let (_, raw) = master(ms_to_frames(1_000, RATE), 3);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 0));
        silence(&mut p, 100, &mut ev);
        feed(&mut p, &raw, &mut ev);
        ev.extend(p.media(Media { playing: false, track: Some(t.clone()), position_ms: 1_000 }));
        assert!(ev.iter().any(|e| matches!(e, Event::Dropped { reason: Reason::Paused, .. })));
    }

    #[test]
    fn stalled_player_discards_the_track() {
        let t = track(2_000);
        let (_, raw) = master(ms_to_frames(500, RATE), 4);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 0));
        silence(&mut p, 100, &mut ev);
        feed(&mut p, &raw, &mut ev);
        silence(&mut p, 10, &mut ev); // a silent packet mid-track = underrun
        assert!(ev.iter().any(|e| matches!(e, Event::Dropped { reason: Reason::Underrun, .. })));
    }

    #[test]
    fn gapless_transition_has_no_clean_start() {
        let a = track(1_000);
        let b = Track { title: "Next".into(), ..track(1_000) };
        let (_, raw) = master(ms_to_frames(8_000, RATE), 5); // continuous audio, never silent
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&a, 0));
        silence(&mut p, 100, &mut ev);
        let one_s = ms_to_frames(1_000, RATE) * 2;
        feed(&mut p, &raw[..one_s], &mut ev); // a completes
        ev.extend(p.media(playing(&b, 0)));
        feed(&mut p, &raw[one_s..], &mut ev); // b starts without silence
        assert!(ev.iter().any(|e| matches!(e, Event::Completed { track, .. } if track.title == "Song")));
        assert!(ev.iter().any(|e| matches!(e, Event::Dropped { track, reason: Reason::NoCleanStart } if track.title == "Next")));
    }

    #[test]
    fn joining_mid_track_is_skipped() {
        let t = track(60_000);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 30_000));
        assert!(ev.is_empty(), "a position beyond the start may be stale for a moment");
        silence(&mut p, 2_100, &mut ev);
        ev.extend(p.media(playing(&t, 32_100)));
        assert!(matches!(ev[..], [Event::Dropped { reason: Reason::MidTrack, .. }]));
        assert!(p.media(playing(&t, 32_200)).is_empty(), "the same track is not reported again");
    }

    #[test]
    fn a_stale_position_settles_and_the_track_is_recorded() {
        // The new title arrives with the previous track's position, then the real one: recorded from the clean start.
        let t = track(1_000);
        let (_, raw) = master(ms_to_frames(1_000, RATE), 9);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = Vec::new();
        silence(&mut p, 1_200, &mut ev);
        ev.extend(p.media(playing(&t, 95_000)));
        feed(&mut p, &raw[..ms_to_frames(300, RATE) * 2], &mut ev);
        ev.extend(p.media(playing(&t, 300)));
        feed(&mut p, &raw[ms_to_frames(300, RATE) * 2..], &mut ev);
        assert!(ev.iter().any(|e| matches!(e, Event::Completed { .. })), "{ev:?}");
    }

    #[test]
    fn a_dropped_track_is_taken_again_when_it_starts_over() {
        let t = track(1_000);
        let (_, raw) = master(ms_to_frames(1_000, RATE), 5);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = Vec::new();
        silence(&mut p, 1_200, &mut ev);
        ev.extend(p.media(playing(&t, 0)));
        feed(&mut p, &raw[..ms_to_frames(400, RATE) * 2], &mut ev);
        ev.extend(p.media(playing(&t, 400)));
        ev.extend(p.media(Media { playing: false, track: Some(t.clone()), position_ms: 400 }));
        assert!(ev.iter().any(|e| matches!(e, Event::Dropped { reason: Reason::Paused, .. })));
        // Back to the start while paused, a moment of silence, then played again from 0:00.
        ev.extend(p.media(Media { playing: false, track: Some(t.clone()), position_ms: 0 }));
        silence(&mut p, 300, &mut ev);
        ev.extend(p.media(playing(&t, 0)));
        feed(&mut p, &raw, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, Event::Completed { .. })), "{ev:?}");
    }

    #[test]
    fn seeking_discards_the_track() {
        let t = track(10_000);
        let (_, raw) = master(ms_to_frames(1_000, RATE), 6);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 0));
        silence(&mut p, 100, &mut ev);
        feed(&mut p, &raw, &mut ev);
        ev.extend(p.media(playing(&t, 7_000))); // position jumped far ahead of what was recorded
        assert!(ev.iter().any(|e| matches!(e, Event::Dropped { reason: Reason::Seeked, .. })));
    }
}

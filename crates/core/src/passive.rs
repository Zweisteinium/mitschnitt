//! Records every track from its first sample to its exact end, and drops anything not provably complete.
//!
//! A track starts at a silence-to-audio edge (Spotify leaves 10 ms of digital silence when a track is started). A
//! lossless source played at gain 0.5 arrives as exact 24-bit values; anything off that grid was altered on the way.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`
pub const FLAG_GLITCH: u32 = 0x1;
/// `AUDCLNT_BUFFERFLAGS_SILENT`: the player delivered no audio.
pub const FLAG_SILENT: u32 = 0x2;

const MIN_SILENCE_MS: u64 = 10;
/// Rendered zeros at the start of the capture prove nothing: the stream warms up with a few ms of zeros even mid-track.
const MIN_INITIAL_SILENCE_MS: u64 = 1_000;
/// A recording dropped this early by a title change was started under stale metadata: its start goes back to the pool.
const RELEASE_START_MS: u64 = 3_000;
/// A master's leading digital silence is kept when the player's stream starts right before it.
const MAX_LEAD_MS: u64 = 3_000;
const LOOKBACK_MS: u64 = 6_000;
const WAIT_MS: u64 = 5_000;
const START_TOLERANCE_MS: i64 = 3_000;
const POSITION_MATCH_MS: i64 = 1_500;
const SEEK_TOLERANCE_MS: i64 = 3_000;
/// A new title may briefly come with the previous track's position.
const POSITION_SETTLE_MS: u64 = 2_000;
/// The player's status runs ahead of its audio: at the end it showed 0:00 again (repeat) while the last samples were
/// still on their way (under 0.1 s), and Spotify announces the next track about 0.3 s early (see the media watcher).
const END_MARGIN_MS: u64 = 500;

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
    /// Extrapolated to now while playing.
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
}

impl Reason {
    pub fn message(self) -> &'static str {
        match self {
            Reason::Paused => "Paused",
            Reason::TrackChanged => "Track changed before its end",
            Reason::Seeked => "Seeked within the track",
            Reason::Underrun => "The player stalled (buffering) – dropout",
            Reason::Glitch => "Capture glitch",
            Reason::VolumeChanged => "App volume changed",
            Reason::Stopped => "Stopped",
            Reason::MidTrack => "Joined mid-track",
            Reason::NoCleanStart => "No silence before the track (gapless) – exact start unknown",
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Started(Track),
    /// `samples` at the original level (24 bit); `off_grid`/`off_span`: frames that weren't exact, the first and last.
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

/// Levels and grid since the last status.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub track: Option<Track>,
    pub recorded_ms: u64,
    pub level: [f32; 2],
    pub rms: [f32; 2],
    /// `None` when only silence arrived.
    pub grid: Option<Grid>,
}

/// Share of non-zero frames on the 24-bit / 16-bit grid.
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
    /// Index of the next frame.
    frame: u64,
    /// The last `LOOKBACK_MS`: frames `[ring_first, frame)`, and per frame whether it was off the grid.
    ring: VecDeque<i32>,
    ring_off: VecDeque<bool>,
    ring_first: u64,
    zero_run: u64,
    zero_from: u64,
    /// The zero run contains "silent" packets: the stream wasn't rendering, so it is real silence.
    zero_silent: bool,
    silent_run: u64,
    /// Where the player's stream last started rendering.
    render_start: Option<u64>,
    /// Clean starts still inside the ring.
    starts: VecDeque<u64>,
    /// Finished or given up: not taken again until it changes or starts over.
    done: Option<Track>,
    last_position_ms: i64,
    /// A track first shown beyond the start, and since which frame.
    late: Option<(Track, u64)>,
    level: [f32; 2],
    energy: ([f64; 2], u64),
    grid: GridCount,
    scratch: Vec<i32>,
}

impl Passive {
    /// `gain`: the player's app volume, undone on every sample.
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

    /// One captured packet, interleaved.
    pub fn push(&mut self, data: &[f32], frames: usize, flags: u32) -> Vec<Event> {
        let mut events = Vec::new();
        let silent = flags & FLAG_SILENT != 0 || data.len() < frames * self.channels;
        if flags & FLAG_GLITCH != 0 {
            self.drop_recording(Reason::Glitch, &mut events);
        }
        if silent {
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
            if zero {
                if self.zero_run == 0 {
                    (self.zero_from, self.zero_silent) = (self.frame, false);
                }
                self.zero_silent |= silent;
                self.zero_run += 1;
            } else {
                self.grid.frames += 1;
                self.grid.off24 += off24 as u64;
                self.grid.off16 += off16 as u64;
                let initial = self.zero_from == 0 && !self.zero_silent && self.zero_run < ms_to_frames(MIN_INITIAL_SILENCE_MS, self.rate) as u64;
                if self.zero_run >= min_silence && !initial {
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

    pub fn media(&mut self, m: Media) -> Vec<Event> {
        let mut events = Vec::new();
        match &self.state {
            // A real pause at the end still ends it: the audio stops (silent packets, an underrun).
            State::Recording { track, expected, samples, .. } if frames_to_ms(expected.saturating_sub(samples.len() / self.channels), self.rate) > END_MARGIN_MS => {
                let recorded = samples.len() / self.channels;
                let (elapsed_ms, left_ms) = (frames_to_ms(recorded, self.rate) as i64, frames_to_ms(expected - recorded, self.rate));
                // No track at all: its metadata is being updated (seen 35 s before the end); a real change shows the
                // other track within a moment.
                if !m.playing {
                    crate::log!("paused with {left_ms} ms left");
                    self.drop_recording(Reason::Paused, &mut events);
                } else if let Some(shown) = m.track.as_ref().filter(|t| *t != track) {
                    crate::log!("track changed with {left_ms} ms left: recording {track:?}, the player shows {shown:?} at {} ms", m.position_ms);
                    self.drop_recording(Reason::TrackChanged, &mut events);
                } else if m.track.is_some() && (m.position_ms - elapsed_ms).abs() > SEEK_TOLERANCE_MS {
                    crate::log!("seeked with {left_ms} ms left: recorded {elapsed_ms} ms, the player is at {} ms", m.position_ms);
                    self.drop_recording(Reason::Seeked, &mut events);
                }
            }
            State::Waiting { track, .. } => {
                if !m.playing || m.track.as_ref() != Some(track) {
                    self.state = State::Idle;
                }
            }
            State::Recording { .. } | State::Idle => {}
        }

        if let (State::Idle, true, Some(track)) = (&self.state, m.playing, &m.track) {
            // Started over: a finished or dropped track may be taken again (the margin absorbs position jitter).
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

    /// Something outside the stream spoilt the recording (volume, devices).
    pub fn interrupt(&mut self, reason: Reason) -> Vec<Event> {
        let mut events = Vec::new();
        self.drop_recording(reason, &mut events);
        events
    }

    pub fn stop(&mut self) -> Vec<Event> {
        let events = self.interrupt(Reason::Stopped);
        self.state = State::Idle;
        events
    }

    pub fn recording(&self) -> bool {
        matches!(self.state, State::Recording { .. })
    }

    pub fn recorded_ms(&self) -> u64 {
        match &self.state {
            State::Recording { samples, .. } => frames_to_ms(samples.len() / self.channels, self.rate),
            _ => 0,
        }
    }

    /// Resets the level meter.
    pub fn status(&mut self) -> Status {
        let (phase, track) = match &self.state {
            State::Idle => (Phase::Idle, None),
            State::Waiting { track, .. } => (Phase::Waiting, Some(track.clone())),
            State::Recording { track, .. } => (Phase::Recording, Some(track.clone())),
        };
        let level = std::mem::take(&mut self.level);
        let (sums, frames) = std::mem::take(&mut self.energy);
        let rms = sums.map(|s| if frames > 0 { (s / frames as f64).sqrt() as f32 } else { 0.0 });
        Status { phase, track, recorded_ms: self.recorded_ms(), level, rms, grid: self.grid.take() }
    }

    fn consider_start(&mut self, track: Track, position_ms: i64, events: &mut Vec<Event>) {
        if track.duration_ms == 0 {
            return;
        }
        if position_ms > START_TOLERANCE_MS {
            let since = match &self.late {
                Some((t, frame)) if *t == track => *frame,
                _ => {
                    self.late = Some((track.clone(), self.frame));
                    self.frame
                }
            };
            if self.frame - since >= ms_to_frames(POSITION_SETTLE_MS, self.rate) as u64 {
                self.late = None;
                self.done = Some(track.clone());
                events.push(Event::Dropped { track, reason: Reason::MidTrack });
            }
            return;
        }
        self.late = None;
        // The audio may already be running: its clean start lies `position` ago.
        let target = self.frame as i64 - ms_to_frames(position_ms.max(0) as u64, self.rate) as i64;
        let tolerance = ms_to_frames(POSITION_MATCH_MS as u64, self.rate) as i64;
        match self.starts.iter().rev().find(|s| (**s as i64 - target).abs() <= tolerance) {
            Some(&start) => self.start_recording(track, start, events),
            None => self.state = State::Waiting { track, until: self.frame + ms_to_frames(WAIT_MS, self.rate) as u64 },
        }
    }

    fn start_recording(&mut self, track: Track, start: u64, events: &mut Vec<Event>) {
        let expected = ms_to_frames(track.duration_ms, self.rate);
        let mut samples = Vec::with_capacity(expected * self.channels);
        let first = (start - self.ring_first) as usize;
        samples.extend(self.ring.range(first * self.channels..));
        let off: Vec<usize> = self.ring_off.range(first..).enumerate().filter(|(_, o)| **o).map(|(i, _)| i).collect();
        let (off_grid, off_span) = (off.len(), off.first().zip(off.last()).map(|(a, b)| (*a, *b)));
        // A start belongs to exactly one track.
        self.starts.retain(|s| *s > start);
        events.push(Event::Started(track.clone()));
        let complete = samples.len() >= expected * self.channels;
        self.state = State::Recording { track, expected, samples, off_grid, off_span, start };
        if complete {
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
        if !self.recording() {
            return;
        }
        let State::Recording { track, samples, start, .. } = std::mem::replace(&mut self.state, State::Idle) else { unreachable!() };
        let early = frames_to_ms(samples.len() / self.channels, self.rate) < RELEASE_START_MS;
        if reason == Reason::TrackChanged && early && start >= self.ring_first {
            let at = self.starts.partition_point(|s| *s < start);
            self.starts.insert(at, start);
        }
        self.done = Some(track.clone());
        events.push(Event::Dropped { track, reason });
    }

    /// Undoes the gain and rounds to 24 bit; `true` when the value already was exact.
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
        let raw: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
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
    fn pause_stall_and_seek_discard_the_track() {
        let t = track(10_000);
        let (_, raw) = master(ms_to_frames(1_000, RATE), 3);
        type Interrupt = fn(&mut Passive, &Track) -> Vec<Event>;
        let interruptions: [(Interrupt, Reason); 3] = [
            (|p, t| p.media(Media { playing: false, track: Some(t.clone()), position_ms: 1_000 }), Reason::Paused),
            (|p, _| p.push(&[], 80, FLAG_SILENT), Reason::Underrun),
            (|p, t| p.media(playing(t, 7_000)), Reason::Seeked),
        ];
        for (interrupt, reason) in interruptions {
            let mut p = Passive::new(RATE, 2, GAIN);
            let mut ev = p.media(playing(&t, 0));
            silence(&mut p, 100, &mut ev);
            feed(&mut p, &raw, &mut ev);
            ev.extend(interrupt(&mut p, &t));
            assert!(ev.iter().any(|e| matches!(e, Event::Dropped { reason: r, .. } if *r == reason)), "{reason:?}: {ev:?}");
        }
    }

    /// Both seen in a real run: the title missing for a moment mid-track, and the song repeating at its end. A real
    /// pause at the end still costs the recording.
    #[test]
    fn a_missing_title_or_a_restart_at_the_end_keeps_the_recording() {
        let t = track(3_000);
        let (_, raw) = master(ms_to_frames(3_000, RATE), 5);
        let at = |ms| ms_to_frames(ms, RATE) * 2;
        for paused in [false, true] {
            let mut p = Passive::new(RATE, 2, GAIN);
            let mut ev = p.media(playing(&t, 0));
            silence(&mut p, 100, &mut ev);
            feed(&mut p, &raw[..at(1_000)], &mut ev);
            ev.extend(p.media(Media { playing: true, track: None, position_ms: 1_000 }));
            feed(&mut p, &raw[at(1_000)..at(2_900)], &mut ev);
            ev.extend(p.media(Media { playing: !paused, track: Some(t.clone()), position_ms: 0 }));
            if paused {
                silence(&mut p, 50, &mut ev);
            }
            feed(&mut p, &raw[at(2_900)..], &mut ev);
            let completed = ev.iter().any(|e| matches!(e, Event::Completed { .. }));
            assert_eq!(completed, !paused, "{ev:?}");
        }
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
}

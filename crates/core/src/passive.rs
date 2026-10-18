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
/// No packets arrived for a while (Linux): unlike silence the player rendered, sound may have been due.
pub const FLAG_GAP: u32 = 0x4;

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
/// Where the reported length ends, the song's sound may still go on (up to 10.9 ms measured, Linux): the recording ends
/// where [`END_SILENCE_MS`] of digital silence begins, searched this far past the length. None there means the next
/// song or a skip followed: the end is unproven.
const END_SEARCH_MS: u64 = 50;
/// Spotify pauses at least 0.5 s between songs and before a repeat, always as rendered digital zeros (18 of 18,
/// Linux); a pause inside a song this long right at its listed end, with sound after it, would be cut. A gap in the
/// stream (a stall) is never taken for it: sound may follow.
// TODO(windows): measured on Linux only.
const END_SILENCE_MS: u64 = 200;
/// The next title comes earlier still. Measured on Linux (Spotify 1.2.96): 0.12 to 0.18 s before the end when the next
/// song is from another album (digital silence follows), 1.22 to 1.52 s when it follows gaplessly on the same album.
/// A skip that late looks the same (neither the position nor the audio tells them apart), so a skip in the last 2 s
/// keeps the new song's first moments: accepted, as a gapless album would otherwise lose every song.
// Windows (SMTC): 1.17 to 1.8 s before the end into a gapless next track (3 measured).
const EARLY_ANNOUNCE_MS: u64 = 2_000;
/// Spotify's crossfade mixes a song's last seconds into what follows: with repeat-one it showed 0:00 9.4 s before the
/// listed end, and its mix (nearly every frame off the grid, for 9 s) reached us 1 s later. A seek to 0:00 left 35 ms
/// of fade (2 of 2, Linux, Spotify 1.3.4; 100 ms in earlier runs). A jump is judged on the audio that follows it.
const JUDGE_MS: u64 = 2_500;
const CROSSFADE_OFF_MS: u64 = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

impl Track {
    pub fn same_song(&self, other: &Track) -> bool {
        (&self.title, &self.artist, &self.album) == (&other.title, &other.artist, &other.album)
    }
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
    Crossfade,
}

impl Reason {
    pub fn message(self) -> &'static str {
        match self {
            Reason::Paused => "paused",
            Reason::TrackChanged => "another song started",
            Reason::Seeked => "skipped within the song",
            Reason::Underrun => "Spotify stalled",
            Reason::Glitch => "capture glitch",
            Reason::VolumeChanged => "volume changed",
            Reason::Stopped => "stopped",
            Reason::MidTrack => "started mid-song",
            Reason::NoCleanStart => "no clean start (gapless)",
            Reason::Crossfade => "Spotify's crossfade is on",
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Started(Track),
    /// `samples` at the original level (24 bit); `off_grid`/`off_span`: frames that weren't exact, the first and last.
    /// `end_proven`: digital silence followed within [`END_SEARCH_MS`], so no sound is cut and nothing else follows.
    Completed { track: Track, samples: Vec<i32>, off_grid: usize, off_span: Option<(usize, usize)>, end_proven: bool },
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
    /// `target`: where the clean start should lie (from the shown position).
    Waiting { track: Track, until: u64, target: i64 },
    /// `tail_off`: off-grid frames past `expected`, counted only if the end search keeps them. `gap`: the first frame
    /// past `expected` without packets; silence from there on proves no end. `tail_empty`: per frame past `expected`,
    /// whether the player rendered it as an empty buffer. `jump`: the frame and off-grid count where the player jumped,
    /// while the audio after it is judged ([`JUDGE_MS`]).
    Recording { track: Track, expected: usize, samples: Vec<i32>, off_grid: usize, off_span: Option<(usize, usize)>, tail_off: Vec<usize>, tail_empty: Vec<bool>, gap: Option<usize>, start: u64, jump: Option<(usize, usize)> },
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
        if silent && !self.ending() {
            self.drop_recording(Reason::Underrun, &mut events);
        }
        let gap_packet = flags & FLAG_GAP != 0;
        if let (true, State::Recording { samples, gap, .. }) = (gap_packet, &mut self.state) {
            gap.get_or_insert(samples.len() / self.channels);
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
        let (rate, judge, crossfade_off) = (self.rate, ms_to_frames(JUDGE_MS, self.rate), ms_to_frames(CROSSFADE_OFF_MS, self.rate));
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

            if let State::Recording { expected, samples, off_grid, off_span, tail_off, tail_empty, jump, .. } = &mut self.state {
                samples.extend_from_slice(&self.scratch);
                let i = samples.len() / ch - 1;
                if i >= *expected {
                    tail_empty.push(silent);
                }
                if off24 && i < *expected {
                    *off_grid += 1;
                    *off_span = Some(off_span.map_or((i, i), |(a, _)| (a, i)));
                } else if off24 {
                    tail_off.push(i);
                }
                if let Some((at, off)) = *jump
                    && i + 1 >= at + judge
                {
                    let after = *off_grid + tail_off.len() - off;
                    crate::log!("after the jump: {} ms off the grid", frames_to_ms(after, rate));
                    self.drop_recording(if after >= crossfade_off { Reason::Crossfade } else { Reason::Seeked }, &mut events);
                } else if jump.is_none() && samples.len() >= (*expected + self.end_search()) * ch {
                    self.complete(&mut events);
                }
            }
            self.frame += 1;
        }

        if let State::Waiting { until, .. } = self.state && self.frame >= until {
            let State::Waiting { track, target, .. } = std::mem::replace(&mut self.state, State::Idle) else { unreachable!() };
            let off: Vec<String> = self.starts.iter().map(|s| format!("{:+}", (*s as i64 - target) * 1000 / self.rate as i64)).collect();
            crate::log!("no clean start for \"{}\": silence ended {} ms from where the position puts its start", track.title, if off.is_empty() { "nowhere".into() } else { off.join(", ") });
            self.done = Some(track.clone());
            events.push(Event::Dropped { track, reason: Reason::NoCleanStart });
        }
        events
    }

    pub fn media(&mut self, m: Media) -> Vec<Event> {
        let mut events = Vec::new();
        // Spotify corrected the length of the playing track 1.5 s in (239000 -> 239370 ms, measured).
        if let (State::Recording { track, expected, .. }, Some(shown)) = (&mut self.state, &m.track)
            && shown != track
            && shown.same_song(track)
        {
            crate::log!("length of \"{}\" changed from {} to {} ms", track.title, track.duration_ms, shown.duration_ms);
            *expected = ms_to_frames(shown.duration_ms, self.rate);
            *track = shown.clone();
        }
        match &self.state {
            // A real pause at the end still ends it: the audio stops (silent packets, an underrun).
            State::Recording { track, expected, samples, off_grid, tail_off, jump, .. } if frames_to_ms(expected.saturating_sub(samples.len() / self.channels), self.rate) > END_MARGIN_MS => {
                let recorded = samples.len() / self.channels;
                let (elapsed_ms, left_ms) = (frames_to_ms(recorded, self.rate) as i64, frames_to_ms(expected - recorded, self.rate));
                // No track at all: its metadata is being updated (seen 35 s before the end); a real change shows the
                // other track within a moment.
                if !m.playing {
                    crate::log!("paused with {left_ms} ms left");
                    self.drop_recording(Reason::Paused, &mut events);
                } else if let Some(shown) = m.track.as_ref().filter(|t| *t != track) {
                    if left_ms > EARLY_ANNOUNCE_MS {
                        crate::log!("track changed with {left_ms} ms left: recording {track:?}, the player shows {shown:?} at {} ms", m.position_ms);
                        self.drop_recording(Reason::TrackChanged, &mut events);
                    }
                } else if m.track.is_some() && jump.is_none() && (m.position_ms - elapsed_ms).abs() > SEEK_TOLERANCE_MS {
                    crate::log!("seeked with {left_ms} ms left: recorded {elapsed_ms} ms, the player is at {} ms", m.position_ms);
                    // Audio off the grid already (a lossy stream, the app volume moved) can't show a crossfade.
                    let judge = ms_to_frames(JUDGE_MS, self.rate);
                    let before = self.ring_off.iter().rev().take(judge).filter(|o| **o).count();
                    if before >= ms_to_frames(CROSSFADE_OFF_MS, self.rate) {
                        self.drop_recording(Reason::Seeked, &mut events);
                    } else {
                        let mark = (recorded, *off_grid + tail_off.len());
                        if let State::Recording { jump, .. } = &mut self.state {
                            *jump = Some(mark);
                        }
                    }
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

    /// The player is told to start a song: an earlier clean start can't be its. Measured: the repeat-one restart of the
    /// song before played for a moment before the pause took effect, and its start was taken for the next song's.
    pub fn forget_starts(&mut self) {
        self.starts.clear();
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
            None => self.state = State::Waiting { track, until: self.frame + ms_to_frames(WAIT_MS, self.rate) as u64, target },
        }
    }

    fn start_recording(&mut self, track: Track, start: u64, events: &mut Vec<Event>) {
        let expected = ms_to_frames(track.duration_ms, self.rate);
        let mut samples = Vec::with_capacity(expected * self.channels);
        let first = (start - self.ring_first) as usize;
        samples.extend(self.ring.range(first * self.channels..));
        let off: Vec<usize> = self.ring_off.range(first..).enumerate().filter(|(_, o)| **o).map(|(i, _)| i).collect();
        let (off, tail_off): (Vec<usize>, Vec<usize>) = off.into_iter().partition(|i| *i < expected);
        let (off_grid, off_span) = (off.len(), off.first().zip(off.last()).map(|(a, b)| (*a, *b)));
        // A start belongs to exactly one track.
        self.starts.retain(|s| *s > start);
        events.push(Event::Started(track.clone()));
        let complete = samples.len() >= (expected + self.end_search()) * self.channels;
        let tail_empty = vec![false; (samples.len() / self.channels).saturating_sub(expected)];
        self.state = State::Recording { track, expected, samples, off_grid, off_span, tail_off, tail_empty, gap: None, start, jump: None };
        if complete {
            self.complete(events);
        }
    }

    /// Frames read past the reported length before the end is decided.
    fn end_search(&self) -> usize {
        ms_to_frames(END_SEARCH_MS + END_SILENCE_MS, self.rate)
    }

    /// Past the reported length, waiting for the end to show.
    pub fn ending(&self) -> bool {
        matches!(&self.state, State::Recording { expected, samples, .. } if samples.len() >= *expected * self.channels)
    }

    /// The player stopped after the song and its stream with it (Linux sends nothing then): the rest is silence.
    pub fn stream_stopped(&mut self) -> Vec<Event> {
        match &self.state {
            State::Recording { expected, samples, .. } if self.ending() => {
                let missing = (expected + self.end_search()).saturating_sub(samples.len() / self.channels);
                self.push(&vec![0.0; missing * self.channels], missing, 0)
            }
            _ => Vec::new(),
        }
    }

    fn complete(&mut self, events: &mut Vec<Event>) {
        if let State::Recording { track, expected, mut samples, mut off_grid, mut off_span, tail_off, tail_empty, gap, .. } = std::mem::replace(&mut self.state, State::Idle) {
            let (ch, min, played) = (self.channels, ms_to_frames(END_SILENCE_MS, self.rate), ms_to_frames(MIN_SILENCE_MS, self.rate));
            let before_gap = gap.unwrap_or(usize::MAX);
            // Spotify's pause may begin as an empty buffer (11.6 ms seen), but it ends in played zeros: a late buffer
            // followed by sound does not.
            let empty = |f: usize| tail_empty.get(f - expected).copied().unwrap_or(false);
            let silent_from = |f: usize| {
                f + min <= before_gap
                    && samples.get(f * ch..(f + min) * ch).is_some_and(|s| s.iter().all(|v| *v == 0))
                    && (f + min - played..f + min).all(|i| !empty(i))
            };
            let end = (expected..=expected + ms_to_frames(END_SEARCH_MS, self.rate)).find(|f| silent_from(*f));
            let cut = end.unwrap_or(expected);
            samples.truncate(cut * ch);
            for i in tail_off.into_iter().filter(|i| *i < cut) {
                off_grid += 1;
                off_span = Some(off_span.map_or((i, i), |(a, _)| (a, i)));
            }
            crate::log!("end of {:?}: {}", track.title, match end {
                Some(f) => format!("silence {} ms past the reported length", frames_to_ms(f - expected, self.rate)),
                None => "no silence after the reported length".into(),
            });
            self.done = Some(track.clone());
            events.push(Event::Completed { track, samples, off_grid, off_span, end_proven: end.is_some() });
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

    fn zeros(p: &mut Passive, ms: u64, events: &mut Vec<Event>) {
        let frames = ms_to_frames(ms, RATE);
        events.extend(p.push(&vec![0.0; frames * 2], frames, 0));
    }

    fn completed(ev: &[Event]) -> (&Vec<i32>, bool) {
        ev.iter().find_map(|e| match e { Event::Completed { samples, end_proven, .. } => Some((samples, *end_proven)), _ => None }).expect("completed")
    }

    /// Measured on Linux: a song's sound went on 10.9 ms past its listed length, then Spotify's pause began.
    #[test]
    fn sound_past_the_listed_length_is_kept_up_to_the_pause() {
        let t = track(1_000);
        let (orig, raw) = master(ms_to_frames(1_011, RATE), 12);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 0));
        silence(&mut p, 100, &mut ev);
        feed(&mut p, &raw, &mut ev);
        zeros(&mut p, 500, &mut ev);
        let (samples, proven) = completed(&ev);
        assert!(proven);
        assert_eq!(samples[..], orig[..], "every sample of sound, nothing cut");
    }

    #[test]
    fn the_songs_own_silence_at_the_end_proves_it() {
        let t = track(1_000);
        let (orig, raw) = master(ms_to_frames(900, RATE), 13);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 0));
        silence(&mut p, 100, &mut ev);
        feed(&mut p, &raw, &mut ev);
        zeros(&mut p, 600, &mut ev); // the master's last 100 ms are silent, then the pause
        let (samples, proven) = completed(&ev);
        assert!(proven);
        assert_eq!(samples.len(), ms_to_frames(1_000, RATE) * 2);
        assert_eq!(samples[..orig.len()], orig[..]);
    }

    /// A pause inside the song right at its listed length, with sound after it, is not the end.
    #[test]
    fn a_short_pause_at_the_listed_end_does_not_prove_it() {
        let t = track(1_000);
        let (_, raw) = master(ms_to_frames(1_000, RATE), 14);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 0));
        silence(&mut p, 100, &mut ev);
        feed(&mut p, &raw, &mut ev);
        zeros(&mut p, 50, &mut ev);
        feed(&mut p, &raw[..ms_to_frames(300, RATE) * 2], &mut ev);
        zeros(&mut p, 500, &mut ev);
        let (samples, proven) = completed(&ev);
        assert!(!proven);
        assert_eq!(samples.len(), ms_to_frames(1_000, RATE) * 2);
    }

    /// Measured: Spotify's pause is rendered zeros, a gap only comes with a stall, pause or seek.
    #[test]
    fn a_stall_after_the_listed_length_proves_no_end() {
        let t = track(1_000);
        let (_, raw) = master(ms_to_frames(1_000, RATE), 16);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 0));
        silence(&mut p, 100, &mut ev);
        feed(&mut p, &raw, &mut ev);
        ev.extend(p.push(&[], ms_to_frames(300, RATE), FLAG_SILENT | FLAG_GAP));
        let (samples, proven) = completed(&ev);
        assert!(!proven);
        assert_eq!(samples.len(), ms_to_frames(1_000, RATE) * 2);
    }

    /// Seen at the end of two songs: the pause began as 512 frames of empty buffers, then played zeros.
    #[test]
    fn a_pause_that_begins_as_an_empty_buffer_proves_the_end() {
        let t = track(1_000);
        let (orig, raw) = master(ms_to_frames(1_000, RATE), 18);
        for (empty_ms, then_sound, proven) in [(12, false, true), (300, false, false), (12, true, false)] {
            let mut p = Passive::new(RATE, 2, GAIN);
            let mut ev = p.media(playing(&t, 0));
            silence(&mut p, 100, &mut ev);
            feed(&mut p, &raw, &mut ev);
            silence(&mut p, empty_ms, &mut ev);
            if then_sound {
                feed(&mut p, &raw[..ms_to_frames(300, RATE) * 2], &mut ev);
            }
            zeros(&mut p, 500, &mut ev);
            let (samples, end) = completed(&ev);
            assert_eq!(end, proven, "{empty_ms} ms empty, then sound: {then_sound}");
            assert_eq!(samples[..], orig[..]);
        }
    }

    #[test]
    fn a_start_before_the_player_was_told_to_start_is_not_taken() {
        let (old, new) = (track(1_000), Track { title: "Next".into(), ..track(1_000) });
        let (_, raw) = master(ms_to_frames(1_000, RATE), 17);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&old, 0));
        silence(&mut p, 100, &mut ev);
        feed(&mut p, &raw, &mut ev);
        zeros(&mut p, 500, &mut ev);
        feed(&mut p, &raw[..ms_to_frames(100, RATE) * 2], &mut ev); // the repeat plays for a moment
        silence(&mut p, 1_000, &mut ev);
        p.forget_starts();
        ev.extend(p.media(playing(&new, 0)));
        assert!(!ev.iter().any(|e| matches!(e, Event::Started(t) if t.title == "Next")), "{ev:?}");
        silence(&mut p, 100, &mut ev);
        feed(&mut p, &raw, &mut ev);
        zeros(&mut p, 500, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, Event::Completed { track, end_proven: true, .. } if track.title == "Next")), "{ev:?}");
    }

    #[test]
    fn a_stream_that_stops_after_the_song_ends_it() {
        let t = track(1_000);
        let (orig, raw) = master(ms_to_frames(1_000, RATE), 15);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = p.media(playing(&t, 0));
        silence(&mut p, 100, &mut ev);
        feed(&mut p, &raw, &mut ev);
        assert!(p.ending() && !ev.iter().any(|e| matches!(e, Event::Completed { .. })));
        ev.extend(p.stream_stopped());
        let (samples, proven) = completed(&ev);
        assert!(proven);
        assert_eq!(samples[..], orig[..]);
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
        silence(&mut p, 300, &mut ev);
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
        silence(&mut p, 300, &mut ev);
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
        silence(&mut p, 300, &mut ev);
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
        silence(&mut p, 300, &mut ev);
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
        silence(&mut p, 300, &mut ev);
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
            (|p, t| {
                let mut ev = p.media(playing(t, 7_000));
                feed(p, &master(ms_to_frames(JUDGE_MS, RATE), 4).1, &mut ev);
                ev
            }, Reason::Seeked),
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

    /// Measured (Linux, Spotify 1.3.4): with crossfade the jump to 0:00 came 9.4 s early and nearly every frame after it
    /// was off the grid; a seek to 0:00 left 35 ms of fade.
    #[test]
    fn a_jump_followed_by_a_mix_is_spotifys_crossfade() {
        let t = track(10_000);
        let (_, raw) = master(ms_to_frames(4_000, RATE), 3);
        let off = |ms: u64| master(ms_to_frames(ms, RATE), 5).1.iter().map(|x| x * 0.7).collect::<Vec<f32>>();
        let on = |ms: u64| master(ms_to_frames(ms, RATE), 5).1;
        let dropped = |ev: &[Event]| ev.iter().find_map(|e| match e { Event::Dropped { reason, .. } => Some(*reason), _ => None });
        let cases: [(Vec<f32>, Vec<f32>, Option<Reason>); 3] = [
            (raw.clone(), off(JUDGE_MS), Some(Reason::Crossfade)),
            (raw.clone(), [off(35), on(JUDGE_MS)].concat(), Some(Reason::Seeked)),
            // Off the grid before the jump: a lossy stream can't show a crossfade.
            (off(4_000), Vec::new(), Some(Reason::Seeked)),
        ];
        for (before, after, reason) in cases {
            let mut p = Passive::new(RATE, 2, GAIN);
            let mut ev = p.media(playing(&t, 0));
            silence(&mut p, 100, &mut ev);
            feed(&mut p, &before, &mut ev);
            ev.extend(p.media(playing(&t, 0)));
            let decided_at_once = dropped(&ev).is_some();
            feed(&mut p, &after, &mut ev);
            assert_eq!(dropped(&ev), reason, "{ev:?}");
            assert_eq!(decided_at_once, after.is_empty(), "judged on the audio after the jump");
        }
    }

    /// Both seen in a real run: the title missing for a moment mid-track, and the song repeating at its end. A real
    /// pause at the end still costs the recording.
    /// Seen on Linux: the next title up to 1.52 s before the end. A change earlier than 2 s still ends the recording.
    #[test]
    fn the_next_title_shortly_before_the_end_keeps_the_recording() {
        let (t, next) = (track(3_000), Track { title: "Next".into(), ..track(4_000) });
        let (_, raw) = master(ms_to_frames(3_000, RATE), 6);
        let at = |ms| ms_to_frames(ms, RATE) * 2;
        for (shown_at, completed) in [(1_100, true), (900, false)] {
            let mut p = Passive::new(RATE, 2, GAIN);
            let mut ev = p.media(playing(&t, 0));
            silence(&mut p, 100, &mut ev);
            feed(&mut p, &raw[..at(shown_at)], &mut ev);
            ev.extend(p.media(playing(&next, 0)));
            feed(&mut p, &raw[at(shown_at)..], &mut ev);
            silence(&mut p, 300, &mut ev);
            assert_eq!(ev.iter().any(|e| matches!(e, Event::Completed { .. })), completed, "{ev:?}");
        }
    }

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
            silence(&mut p, 300, &mut ev);
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
        let ending = one_s + ms_to_frames(300, RATE) * 2;
        feed(&mut p, &raw[..one_s], &mut ev);
        ev.extend(p.media(playing(&b, 0)));
        feed(&mut p, &raw[one_s..ending], &mut ev); // a completes, without a pause to prove its end
        ev.extend(p.media(playing(&b, 300)));
        feed(&mut p, &raw[ending..], &mut ev); // b starts without silence
        assert!(ev.iter().any(|e| matches!(e, Event::Completed { track, end_proven: false, .. } if track.title == "Song")));
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
        silence(&mut p, 300, &mut ev);
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
        silence(&mut p, 300, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, Event::Completed { .. })), "{ev:?}");
    }

    #[test]
    fn a_corrected_length_is_taken_over() {
        let (short, long) = (track(2_000), track(3_000));
        let (_, raw) = master(ms_to_frames(3_000, RATE), 5);
        let mut p = Passive::new(RATE, 2, GAIN);
        let mut ev = Vec::new();
        silence(&mut p, 1_200, &mut ev);
        ev.extend(p.media(playing(&short, 0)));
        feed(&mut p, &raw[..ms_to_frames(400, RATE) * 2], &mut ev);
        ev.extend(p.media(playing(&long, 400)));
        feed(&mut p, &raw[ms_to_frames(400, RATE) * 2..], &mut ev);
        silence(&mut p, 300, &mut ev);
        let done = ev.iter().find_map(|e| match e { Event::Completed { track, samples, .. } => Some((track, samples.len())), _ => None });
        assert_eq!(done, Some((&long, raw.len())), "{ev:?}");
    }
}

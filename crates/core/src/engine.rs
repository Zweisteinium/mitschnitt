//! Wires capture, media session and volumes to [`Passive`]. The engine thread owns every platform object (COM/WinRT
//! on Windows); finished tracks are written on worker threads. With a [`Queue`] attached, a [`Runner`] drives the
//! player and only its songs are written.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::analyze::{self, Lossy, Master, Report, Severity};
use crate::audio::Audio;
use crate::capture::{Capture, CaptureEvent, Packet};
use crate::devices::{self, default_endpoint};
use crate::flac::Tags;
use crate::guard::Guard;
use crate::media::{self, Control, Extra, Repeat, Watcher};
use crate::monitor::{Monitor, MonitorState};
use crate::output;
use crate::passive::{Event, Passive, Reason, Status, Track};
use crate::power::KeepAwake;
use crate::profile::Profile;
use crate::runner::{Action, Job, Outcome, Runner, Written};
use crate::settings::{Format, Settings};
use crate::{desktop, init_thread, sessions};

pub use crate::platform::os::GAIN;
const CHANNELS: u16 = 2;
const MEDIA_POLL: Duration = Duration::from_millis(100);
/// No packets this long past a song's listed length while the player shows it stopped: the stream ended with the song
/// (Linux sends nothing then). While it shows playing, it is a stall: that proves no end.
const STREAM_STOPPED: Duration = Duration::from_millis(300);
const VOLUME_POLL: Duration = Duration::from_millis(500);
/// Queue mode: the player may still be starting.
const FIND_SESSION: Duration = Duration::from_secs(1);

pub struct Config {
    pub profile: Profile,
    pub settings: Settings,
    /// Diagnostics: raw capture and player timeline into this folder.
    pub trace: Option<PathBuf>,
    pub queue: Option<Queue>,
}

pub struct Queue {
    pub jobs: Receiver<Job>,
    pub done: Sender<(Job, Outcome)>,
    pub status: Arc<Mutex<QueueStatus>>,
    pub out_dir: PathBuf,
    pub skip: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Default)]
pub struct QueueStatus {
    pub job: Option<Job>,
    pub recording: bool,
    pub recorded_ms: u64,
    /// The player plays anything.
    pub playing: bool,
    pub lagging: bool,
}

/// `raw.f32` (interleaved as captured, silent packets as zeros) and `events.tsv` (frame, kind, details); replayed by
/// `passive::tests::replay_trace`.
struct Trace {
    raw: std::io::BufWriter<std::fs::File>,
    log: std::io::BufWriter<std::fs::File>,
    frame: u64,
    last_media: String,
}

impl Trace {
    fn open(dir: &std::path::Path) -> std::io::Result<Trace> {
        std::fs::create_dir_all(dir)?;
        let file = |name: &str| std::fs::File::create(dir.join(name)).map(std::io::BufWriter::new);
        Ok(Trace { raw: file("raw.f32")?, log: file("events.tsv")?, frame: 0, last_media: String::new() })
    }

    fn packet(&mut self, p: &Packet, channels: usize) {
        use std::io::Write;
        if p.flags != 0 {
            let _ = writeln!(self.log, "{}\tflags\t{:#x}\t{}", self.frame, p.flags, p.frames);
        }
        let n = p.frames * channels;
        let samples = if p.data.len() >= n { &p.data[..n] } else { &[][..] };
        for i in 0..n {
            let _ = self.raw.write_all(&samples.get(i).copied().unwrap_or(0.0).to_le_bytes());
        }
        self.frame += p.frames as u64;
    }

    fn media(&mut self, m: &crate::passive::Media) {
        use std::io::Write;
        let what = match &m.track {
            Some(t) => format!("{}\t{} - {}\t{}", m.playing, t.artist, t.title, t.duration_ms),
            None => format!("{}\t-\t0", m.playing),
        };
        if what != self.last_media {
            let _ = writeln!(self.log, "{}\tmedia\t{what}\t{}", self.frame, m.position_ms);
            self.last_media = what;
        }
    }

    fn note(&mut self, text: &str) {
        use std::io::Write;
        let _ = writeln!(self.log, "{}\tevent\t{text}", self.frame);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub app: String,
    pub device: String,
    pub rate: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Update {
    Status { status: Status },
    Started { track: Track },
    Dropped { track: Track, reason: Reason, message: String },
    Exists { track: Track, file: String },
    /// `file`: `None` when a flagged file was removed. `exact`: every sample arrived untouched. `faded`: all but the
    /// player's fade-in did. `lossy`: saved as MP3 (manual) or uploaded as it came (Echolot judges it itself).
    /// `peak_dbfs`: `None` for digital silence.
    Saved { track: Track, file: Option<String>, size: u64, verdict: Severity, master: Master, exact: bool, faded: bool, lossy: Option<Lossy>, issues: Vec<String>, echolot: bool, frames: u64, peak_dbfs: Option<f64> },
    Failed { track: Track, error: String },
    KeyBlocked { count: u32 },
    /// The default output changed.
    MonitorMuted { muted: bool },
    MonitorVolume { volume: f32 },
    Stopped { error: Option<String> },
}

type Notify = Arc<dyn Fn(Update) + Send + Sync>;

enum Msg {
    Packet(Packet),
    CaptureError(String),
    StopAfterTrack,
    Attach(Queue),
    Detach,
    Written(Written),
    Stop,
}

/// Stops and restores volumes when dropped.
pub struct Engine {
    tx: Sender<Msg>,
    monitor_volume: Option<Arc<AtomicU32>>,
    monitor_state: Option<Arc<MonitorState>>,
    thread: Option<JoinHandle<()>>,
    info: SourceInfo,
}

impl Engine {
    pub fn start(config: Config, notify: impl Fn(Update) + Send + Sync + 'static) -> Result<(Engine, SourceInfo), String> {
        std::fs::create_dir_all(&config.settings.out_dir).map_err(|e| format!("output folder: {e}"))?;
        let notify: Notify = Arc::new(notify);
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let packets = tx.clone();
        let thread = std::thread::Builder::new()
            .name("engine".into())
            .spawn(move || {
                init_thread();
                let mut config = config;
                let queue = config.queue.take();
                match Session::open(&config, queue.is_some(), packets) {
                    Ok(mut session) => {
                        let SourceInfo { app, device, rate } = &session.info;
                        crate::log!("engine start {app} on \"{device}\" at {rate} Hz, queue={}", queue.is_some());
                        let monitor = session.monitor.as_ref().map(|m| (m.volume_handle(), m.state()));
                        let _ = ready_tx.send(Ok((session.info.clone(), monitor)));
                        if let Some(queue) = queue {
                            session.attach(queue);
                        }
                        session.run(&rx, &config.settings, &notify);
                    }
                    Err(e) => {
                        crate::log!("engine start failed: {e}");
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let (info, monitor) = ready_rx.recv().map_err(|e| e.to_string())??;
        let (monitor_volume, monitor_state) = monitor.unzip();
        Ok((Engine { tx, monitor_volume, monitor_state, thread: Some(thread), info: info.clone() }, info))
    }

    pub fn info(&self) -> &SourceInfo {
        &self.info
    }

    pub fn stop_after_track(&self) {
        let _ = self.tx.send(Msg::StopAfterTrack);
    }

    /// Gives the queue back when the engine has stopped meanwhile.
    pub fn attach(&self, queue: Queue) -> Result<(), Queue> {
        match self.tx.send(Msg::Attach(queue)) {
            Ok(()) => Ok(()),
            Err(mpsc::SendError(Msg::Attach(queue))) => Err(queue),
            Err(_) => unreachable!("sent an Attach"),
        }
    }

    pub fn detach(&self) {
        let _ = self.tx.send(Msg::Detach);
    }

    /// The current output becomes the monitor's device.
    pub fn unmute_monitor(&self) {
        if let Some(s) = &self.monitor_state {
            s.unmute();
        }
    }

    pub fn set_monitor_volume(&self, volume: f32) {
        if let Some(v) = &self.monitor_volume {
            v.store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the engine changed, persisted so a crash is repaired on the next start ([`recover`]). Controls are looked up
/// again when applied: a restarted app has new sessions.
#[derive(Serialize, Deserialize)]
struct Restore {
    app: String,
    app_volume: Option<f32>,
    /// We muted "Listen to this device".
    unmute_listen: bool,
    rate: Option<u32>,
    /// The player's repeat mode before a queue song switched it to repeat-one.
    #[serde(default)]
    repeat: Option<Repeat>,
}

impl Restore {
    fn path() -> PathBuf {
        crate::settings::state_dir().join("restore.json")
    }

    fn save(&self) {
        let path = Self::path();
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
        let _ = std::fs::write(&path, serde_json::to_string(self).unwrap_or_default());
    }

    fn load() -> Option<Restore> {
        serde_json::from_str(&std::fs::read_to_string(Self::path()).ok()?).ok()
    }

    fn apply(&self) {
        if let Some(volume) = self.app_volume {
            sessions::set(&sessions::app_volumes_by_name(&self.app), volume);
        }
        if self.unmute_listen {
            sessions::set_mute(&sessions::listen_volumes(), false);
        }
        if let Some(rate) = self.rate {
            devices::release_rate(rate);
        }
        if let Some(repeat) = self.repeat
            && let Some(profile) = crate::profile::PROFILES.iter().find(|p| p.exe == self.app)
            && let Ok(Some(session)) = media::find(profile)
        {
            let _ = Watcher::new(session).set_repeat(repeat);
        }
        let _ = std::fs::remove_file(Self::path());
    }
}

/// Undoes what a crashed or killed run left behind.
pub fn recover() {
    if let Some(restore) = Restore::load() {
        init_thread();
        restore.apply();
    }
}

/// Where an app without an audio stream yet will play: the cable, at the app's rate.
fn expected_endpoint(profile: &Profile) -> Result<devices::Endpoint, String> {
    let default = default_endpoint().map_err(|e| e.to_string())?;
    if profile.rate == 0 {
        return Ok(default);
    }
    let cable = devices::render_endpoints().ok().into_iter().flatten().find(|e| e.name.contains("CABLE Input"));
    Ok(cable.unwrap_or(devices::Endpoint { sample_rate: profile.rate, ..default }))
}

struct QueueRun {
    queue: Queue,
    runner: Runner,
    /// The monitor is silenced while the queue runs: the level to go back to.
    quiet: Option<f32>,
    /// The runner's stage as last logged.
    stage: &'static str,
    /// What the player showed as last logged while a song starts: a failed start is explained by it.
    shown: (bool, Option<crate::passive::Track>),
    track: Option<crate::passive::Track>,
}

impl QueueRun {
    fn log_stage(&mut self, media: Option<&crate::passive::Media>) -> Option<&'static str> {
        let stage = self.runner.stage();
        if let Some(t) = media.and_then(|m| m.track.clone()) {
            self.track = Some(t);
        }
        if let Some(m) = media
            && matches!(stage, "Starting" | "Playing" | "Rewind")
            && self.shown != (m.playing, m.track.clone())
        {
            crate::log!("player {}", show(m));
            self.shown = (m.playing, m.track.clone());
        }
        if stage == self.stage {
            return None;
        }
        crate::log!("runner {} -> {stage}{}", self.stage, media.map(|m| format!(" | {}", show(m))).unwrap_or_default());
        self.stage = stage;
        Some(stage)
    }
}

fn show(m: &crate::passive::Media) -> String {
    let track = m.track.as_ref().map_or("-".into(), |t| format!("\"{} - {}\"", t.artist, t.title));
    format!("{} {track} @{:.1}s", if m.playing { "playing" } else { "paused" }, m.position_ms as f64 / 1000.0)
}

/// Everything on the engine thread.
struct Session {
    info: SourceInfo,
    profile: Profile,
    root_pid: u32,
    /// In queue mode it may only appear once the first song plays.
    watcher: Option<Watcher>,
    find_at: Instant,
    /// Read when the current recording started.
    extra: Option<(Track, Extra)>,
    trace: Option<Trace>,
    guard: Option<Guard>,
    restore: Restore,
    passive: Passive,
    monitor: Option<Monitor>,
    queue: Option<QueueRun>,
    tx: Sender<Msg>,
    _capture: Capture,
}

impl Session {
    fn open(config: &Config, queued: bool, packets: Sender<Msg>) -> Result<Session, String> {
        recover();
        let profile = &config.profile;
        let session = media::find(profile).map_err(|e| e.to_string())?;
        if session.is_none() && !queued {
            return Err(format!("{} shows no playback yet – start playback in the app first.", profile.name));
        }
        let procs = sessions::processes();
        let root = sessions::find_app(profile.exe, &procs).ok_or_else(|| format!("{} is not running.", profile.name))?;
        let tree = sessions::tree(root.pid, &procs);
        let monitoring = config.settings.monitor && crate::monitor::SUPPORTED;
        let listen = sessions::listen_volumes();
        let unmute_listen = monitoring && sessions::muted(&listen).iter().any(|m| !m);
        // Without an audio stream yet (Linux players open one on play), the volume is learnt when it appears.
        let app_controls = sessions::app_volumes(&tree);
        let app_volume = sessions::get(&app_controls).into_iter().reduce(f32::max);
        let mut restore = Restore { app: profile.exe.to_owned(), app_volume, unmute_listen, rate: None, repeat: None };
        restore.save();
        let mut endpoint = match sessions::app_endpoint(&tree) {
            Some(endpoint) => endpoint,
            None => expected_endpoint(profile)?,
        };
        if profile.rate > 0 {
            match devices::hold_rate(profile.rate) {
                Ok(previous) => restore.rate = previous,
                Err(e) => {
                    restore.apply();
                    return Err(e);
                }
            }
        }
        if restore.rate.is_some() {
            endpoint.sample_rate = profile.rate;
            restore.save();
        }
        sessions::set(&app_controls, GAIN);
        if unmute_listen {
            sessions::set_mute(&listen, true); // the monitor replaces it
        }

        let tx = packets.clone();
        let capture = Capture::start(root.pid, endpoint.sample_rate, CHANNELS, move |event| {
            let _ = packets.send(match event {
                CaptureEvent::Packet(p) => Msg::Packet(p),
                CaptureEvent::Error(e) => Msg::CaptureError(e),
            });
        });
        let capture = match capture {
            Ok(c) => c,
            Err(e) => {
                restore.apply();
                return Err(e);
            }
        };
        let monitor = crate::monitor::SUPPORTED.then(|| Monitor::start(endpoint.sample_rate, CHANNELS, GAIN, root.pid, monitoring)).flatten();
        Ok(Session {
            info: SourceInfo { app: profile.name.to_owned(), device: endpoint.name, rate: endpoint.sample_rate },
            profile: profile.clone(),
            root_pid: root.pid,
            watcher: session.map(Watcher::new),
            find_at: Instant::now(),
            extra: None,
            guard: config.settings.protect_media_keys.then(Guard::start).and_then(Result::ok),
            trace: match &config.trace {
                Some(dir) => Some(Trace::open(dir).map_err(|e| format!("trace: {e}"))?),
                None => None,
            },
            restore,
            passive: Passive::new(endpoint.sample_rate, CHANNELS as usize, GAIN),
            monitor,
            queue: None,
            tx,
            _capture: capture,
        })
    }

    fn attach(&mut self, queue: Queue) {
        self.detach(Outcome::Stalled { detail: "replaced by another list".into() });
        self.queue = Some(QueueRun { queue, runner: Runner::new(Instant::now()), quiet: None, stage: "Idle", shown: (false, None), track: None });
    }

    fn detach(&mut self, outcome: Outcome) {
        let Some(mut run) = self.queue.take() else { return };
        let actions = run.runner.abort(Instant::now(), outcome);
        self.act(&actions, run.runner.job());
        for done in run.runner.take_done() {
            let _ = run.queue.done.send(done);
        }
        if let (Some(level), Some(monitor)) = (run.quiet, &self.monitor) {
            // Moved by the user meanwhile: that level stays.
            let handle = monitor.volume_handle();
            if f32::from_bits(handle.load(Ordering::Relaxed)) == 0.0 {
                handle.store(level.to_bits(), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(50)); // let the monitor apply it before it may stop
            }
        }
        if let Ok(mut status) = run.queue.status.lock() {
            *status = QueueStatus::default();
        }
        self.stage_changed("Idle");
    }

    fn act(&mut self, actions: &[Action], job: Option<&Job>) {
        for action in actions {
            if matches!(action, Action::Open | Action::Restart) {
                self.passive.forget_starts();
            }
            if *action == Action::Play
                && let Some(track) = self.queue.as_ref().and_then(|q| q.track.clone()).filter(|t| job.is_some_and(|j| crate::runner::matches(j, t).is_ok()))
            {
                self.passive.arm(track);
            }
            crate::log!("runner {action:?} {}", job.map(Job::label).unwrap_or_default());
            let result = match action {
                Action::Open => job.map_or(Ok(()), |j| desktop::open_link_in_background(&j.play_uri(), self.profile.exe)),
                Action::Play | Action::Pause | Action::Next | Action::Previous | Action::Restart => {
                    let control = match action {
                        Action::Play => Control::Play,
                        Action::Pause => Control::Pause,
                        Action::Next => Control::Next,
                        Action::Previous => Control::Previous,
                        _ => Control::Restart,
                    };
                    self.watcher.as_ref().map_or(Ok(()), |w| w.control(control))
                }
            };
            if let Err(e) = result {
                crate::log!("runner {action:?} failed: {e}");
            }
        }
    }

    fn run(mut self, rx: &mpsc::Receiver<Msg>, settings: &Settings, notify: &Notify) {
        let awake = KeepAwake::start();
        let (mut next_media, mut next_volume) = (Instant::now(), Instant::now() + VOLUME_POLL);
        let (mut error, mut stop_after) = (None, false);
        let (mut blocked, mut muted, mut volume) = (0, false, f32::NAN);
        let (mut last_packet, mut playing) = (Instant::now(), true);
        loop {
            // A queue's song being written still needs the engine for its result.
            if stop_after && !self.passive.recording() && !self.queue.as_ref().is_some_and(|q| q.runner.writing()) {
                break;
            }
            if let Some(guard) = &mut self.guard {
                guard.update(self.passive.recording(), self.extra.as_ref());
            }
            let timeout = next_media.saturating_duration_since(Instant::now());
            let events = match rx.recv_timeout(timeout) {
                Ok(Msg::Packet(p)) => {
                    last_packet = Instant::now();
                    if let Some(t) = &mut self.trace {
                        t.packet(&p, CHANNELS as usize);
                    }
                    if let Some(m) = &self.monitor {
                        m.feed(&p.data, p.frames);
                    }
                    self.passive.push(&p.data, p.frames, p.flags)
                }
                Ok(Msg::CaptureError(e)) => {
                    error = Some(e);
                    break;
                }
                Ok(Msg::StopAfterTrack) => {
                    stop_after = true;
                    Vec::new()
                }
                Ok(Msg::Attach(queue)) => {
                    self.attach(queue);
                    Vec::new()
                }
                Ok(Msg::Detach) => {
                    self.detach(Outcome::Stalled { detail: "stopped".into() });
                    Vec::new()
                }
                Ok(Msg::Written(result)) => {
                    if let Some(run) = &mut self.queue {
                        run.runner.written(Instant::now(), result);
                    }
                    Vec::new()
                }
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => Vec::new(),
            };
            self.dispatch(events, settings, notify);

            let now = Instant::now();
            if !playing && self.passive.ending() && now.duration_since(last_packet) >= STREAM_STOPPED {
                let events = self.passive.stream_stopped();
                self.dispatch(events, settings, notify);
            }
            if now >= next_media {
                next_media = now + MEDIA_POLL;
                if self.watcher.is_none() && now >= self.find_at {
                    self.find_at = now + FIND_SESSION;
                    self.watcher = media::find(&self.profile).ok().flatten().map(Watcher::new);
                    if self.watcher.is_some() {
                        crate::log!("media session found");
                    }
                }
                let snapshot = self.watcher.as_mut().map(|w| w.snapshot().map_err(|e| e.to_string()));
                match snapshot {
                    Some(Ok(mut media)) => {
                        playing = media.playing;
                        media.track = media.track.filter(|t| !(self.profile.is_ad)(t));
                        if let Some(t) = &mut self.trace {
                            t.media(&media);
                        }
                        let events = self.passive.media(media.clone());
                        self.dispatch(events, settings, notify);
                        self.drive(now, &media);
                    }
                    // Gone for a moment, or with the app (on Linux its session goes first; the check below says so).
                    Some(Err(e)) => {
                        crate::log!("media session lost: {e}");
                        self.watcher = None;
                        self.find_at = now;
                    }
                    None => self.drive(now, &crate::passive::Media::default()),
                }
                if now >= next_volume {
                    next_volume = now + VOLUME_POLL;
                    if !sessions::is_running(self.root_pid) {
                        error = Some(format!("{} was closed.", self.info.app));
                        break;
                    }
                    // Windows recreates "Listen to this device" unmuted when the outputs change.
                    if self.monitor.is_some() {
                        let listen = sessions::listen_volumes();
                        if sessions::muted(&listen).iter().any(|m| !m) {
                            sessions::set_mute(&listen, true);
                            if !self.restore.unmute_listen {
                                self.restore.unmute_listen = true;
                                self.restore.save();
                            }
                        }
                    }
                    let controls = sessions::app_volumes_by_name(self.profile.exe);
                    let volumes = sessions::get(&controls);
                    if volumes.iter().any(|v| (v - GAIN).abs() > 1e-4) {
                        crate::log!("app volume {volumes:?}, set back to {GAIN} (recording={})", self.passive.recording());
                        if self.restore.app_volume.is_none() {
                            self.restore.app_volume = volumes.into_iter().reduce(f32::max);
                            self.restore.save();
                        }
                        // Moved by someone, or a new stream: put back so the next tracks are exact again.
                        let events = self.passive.interrupt(Reason::VolumeChanged);
                        self.dispatch(events, settings, notify);
                        sessions::set(&controls, GAIN);
                    }
                }
                notify(Update::Status { status: self.passive.status() });
                if let Some(count) = self.guard.as_ref().map(Guard::blocked).filter(|c| *c != blocked) {
                    blocked = count;
                    crate::log!("media keys blocked: {count}");
                    notify(Update::KeyBlocked { count });
                }
                if let Some(now) = self.monitor.as_ref().map(|m| m.state().muted.load(Ordering::Relaxed)).filter(|m| *m != muted) {
                    muted = now;
                    crate::log!("monitor muted={muted}");
                    notify(Update::MonitorMuted { muted });
                }
                if let Some(now) = self.monitor.as_ref().map(|m| f32::from_bits(m.volume_handle().load(Ordering::Relaxed))).filter(|v| !v.is_nan() && *v != volume) {
                    volume = now;
                    notify(Update::MonitorVolume { volume });
                }
            }
        }
        crate::log!("engine stop: {}", error.as_deref().unwrap_or("by the user"));
        let events = self.passive.stop();
        self.dispatch(events, settings, notify);
        // Stopped by the user: a take-over. A failure: worth another try.
        self.detach(match &error {
            Some(e) => Outcome::Stalled { detail: e.clone() },
            None => Outcome::TookOver { detail: "stopped".into() },
        });
        self.guard = None;
        self.monitor = None;
        self.restore.apply();
        drop(awake);
        notify(Update::Stopped { error });
    }

    /// Queue mode, once per media snapshot.
    fn drive(&mut self, now: Instant, media: &crate::passive::Media) {
        let Some(run) = &mut self.queue else { return };
        if run.quiet.is_none()
            && let Some(monitor) = &self.monitor
        {
            let handle = monitor.volume_handle();
            let level = f32::from_bits(handle.load(Ordering::Relaxed));
            if !level.is_nan() {
                run.quiet = Some(level);
                handle.store(0f32.to_bits(), Ordering::Relaxed);
            }
        }
        let mut actions = run.runner.tick(now, media);
        if run.queue.skip.swap(false, Ordering::Relaxed) {
            if run.runner.job().is_some() {
                actions.extend(run.runner.abort(now, Outcome::Skipped));
            } else if let Ok(job) = run.queue.jobs.try_recv() {
                let _ = run.queue.done.send((job, Outcome::Skipped));
            }
        }
        if run.runner.ready(now)
            && let Ok(job) = run.queue.jobs.try_recv()
        {
            actions.extend(run.runner.assign(job, now, media));
        }
        let changed = run.log_stage(Some(media));
        for d in run.runner.take_done() {
            let _ = run.queue.done.send(d);
        }
        let job = run.runner.job().cloned();
        if let Ok(mut s) = run.queue.status.lock() {
            *s = QueueStatus { job: job.clone(), recording: run.runner.recording(), recorded_ms: self.passive.recorded_ms(), playing: media.playing, lagging: run.runner.lagging() };
        }
        if let Some(stage) = changed {
            self.stage_changed(stage);
        }
        self.act(&actions, job.as_ref());
    }

    fn stage_changed(&mut self, stage: &'static str) {
        if !matches!(stage, "Starting" | "Playing") {
            self.passive.disarm();
        }
        self.sync_repeat(stage);
    }

    /// Repeat-one while a queue song records: no other title then follows early, and the player pauses before the
    /// repeat, which proves the song's end. The user's mode is back between songs, after a take-over and on stop
    /// (after a crash through the restore file). A link switches repeat-one off: it is set again for every song.
    fn sync_repeat(&mut self, stage: &'static str) {
        let Some(watcher) = &self.watcher else { return };
        if stage == "Idle" {
            if let Some(repeat) = self.restore.repeat.take() {
                // Setting it makes Spotify report its position again.
                if watcher.repeat().ok() != Some(repeat)
                    && let Err(e) = watcher.set_repeat(repeat)
                {
                    crate::log!("repeat {repeat:?} failed: {e}");
                }
                self.restore.save();
            }
            return;
        }
        if self.restore.repeat.is_none()
            && let Ok(repeat) = watcher.repeat()
        {
            self.restore.repeat = Some(repeat);
            self.restore.save();
        }
        if stage == "Recording"
            && self.restore.repeat.is_some()
            && let Err(e) = watcher.set_repeat(Repeat::Track)
        {
            crate::log!("repeat-one failed: {e}");
        }
    }

    fn dispatch(&mut self, events: Vec<Event>, settings: &Settings, notify: &Notify) {
        for event in events {
            match &event {
                Event::Started(track) => crate::log!("recording started \"{} - {}\"", track.artist, track.title),
                Event::Dropped { track, reason } => crate::log!("recording dropped \"{} - {}\": {reason:?}", track.artist, track.title),
                Event::Completed { track, off_grid, .. } => crate::log!("recording completed \"{} - {}\", off grid {off_grid}", track.artist, track.title),
            }
            if let Some(t) = &mut self.trace {
                t.note(&match &event {
                    Event::Started(track) => format!("started\t{}", track.title),
                    Event::Dropped { track, reason } => format!("dropped\t{}\t{reason:?}", track.title),
                    Event::Completed { track, off_grid, off_span, .. } => format!("completed\t{}\toff_grid={off_grid}\toff_span={off_span:?}", track.title),
                });
            }
            // Queue mode: only the runner's own songs are reported and written.
            let (mut target, mut changed) = (true, None);
            if let Some(run) = &mut self.queue {
                let now = Instant::now();
                let actions = run.runner.event(now, &event);
                changed = run.log_stage(None);
                let job = run.runner.job().cloned();
                target = match &event {
                    Event::Completed { track, .. } => run.runner.is_target(track),
                    Event::Started(track) | Event::Dropped { track, .. } => job.as_ref().is_some_and(|j| crate::runner::matches(j, track).is_ok()),
                };
                for d in run.runner.take_done() {
                    let _ = run.queue.done.send(d);
                }
                self.act(&actions, job.as_ref());
            }
            if let Some(stage) = changed {
                self.stage_changed(stage);
            }
            match event {
                Event::Started(track) => {
                    let extra = self.watcher.as_ref().map(Watcher::extra).unwrap_or_default();
                    self.extra = Some((track.clone(), extra));
                    if target {
                        notify(Update::Started { track });
                    }
                }
                Event::Dropped { track, reason } => {
                    if target {
                        notify(Update::Dropped { track, reason, message: reason.message().to_owned() })
                    }
                }
                Event::Completed { track, samples, off_grid, off_span, end_proven } => {
                    let extra = self.extra.take().filter(|(t, _)| t.same_song(&track)).map(|(_, e)| e).unwrap_or_default();
                    let mut tags = Tags {
                        title: track.title.clone(),
                        artist: track.artist.clone(),
                        album: track.album.clone(),
                        album_artist: extra.album_artist,
                        track_number: extra.track_number,
                        cover: extra.cover,
                        extra: Vec::new(),
                    };
                    let (rate, lossless, notify) = (self.info.rate, self.profile.lossless, notify.clone());
                    match &self.queue {
                        // The album playing on.
                        Some(_) if !target => {}
                        Some(run) => {
                            let job = run.runner.job().cloned();
                            if let Some(job) = &job {
                                tags.extra = vec![("SPOTIFY_TRACK_ID".into(), job.track_id().into()), ("ISRC".into(), job.isrc.clone())];
                            }
                            let settings = Settings { out_dir: run.queue.out_dir.clone(), format: Format::Flac, keep_flagged: true, skip_existing: false, ..settings.clone() };
                            let (tx, done) = (self.tx.clone(), run.queue.done.clone());
                            std::thread::spawn(move || {
                                let written = finish_job(track, tags, samples, (off_grid, off_span, end_proven), rate, lossless, &settings, &notify);
                                // The engine stopped meanwhile.
                                if let (Err(mpsc::SendError(Msg::Written(written))), Some(job)) = (tx.send(Msg::Written(written)), job) {
                                    let _ = done.send((job, written.into()));
                                }
                            });
                        }
                        None => {
                            let settings = settings.clone();
                            std::thread::spawn(move || finish(track, tags, samples, (off_grid, off_span, end_proven), rate, lossless, &settings, &notify));
                        }
                    }
                }
            }
        }
    }
}

/// What the capture saw: frames off the grid, the first and last of them, and whether silence proved the end.
type Proof = (usize, Option<(usize, usize)>, bool);

/// Only Linux is measured: there Spotify pauses after every song, and the end check needs that pause.
// TODO(windows): without repeat-one Spotify plays on into the next song: 37 of 104 ends had no silence within 50 ms.
// Spotify offers repeat over SMTC (IsRepeatEnabled, 19 Oct); measure its pause before the repeat, then prove the end.
const PROVES_END: bool = cfg!(target_os = "linux");

struct Checked {
    audio: Audio,
    report: Report,
    exact: bool,
    unproven_end: bool,
    /// The note when only the player's fade-in isn't original.
    faded: Option<String>,
    lossy: Option<Lossy>,
}

/// Lossy streams were 99.9 % off the grid; a stuttering start after quick skips was 0.2 % (Schleini, 28 Oct) and got
/// uploaded as "lossy". Anything below this is a glitch, worth another try.
const LOSSY_SHARE: f64 = 0.5;

fn check(samples: Vec<i32>, (off_grid, off_span, end_proven): Proof, rate: u32, lossless: bool) -> Checked {
    let audio = Audio { rate, channels: CHANNELS as usize, bits: 24, samples };
    let exact = off_grid == 0;
    let fade = off_span.filter(|&(_, last)| lossless && last < analyze::fade_in_frames(rate));
    let mut report = analyze::analyze_with(&audio, exact || fade.is_some());
    let mut faded = None;
    let share = off_grid as f64 / audio.frames().max(1) as f64;
    let stream = lossless && !exact && fade.is_none() && share > LOSSY_SHARE;
    let lossy = stream.then_some(Lossy::Stream);
    if !exact && lossless {
        let text = match fade {
            Some((_, last)) => {
                let text = format!("Spotify's fade-in: first {}", crate::duration((last + 1) as f64 * 1000.0 / rate as f64));
                faded = Some(text.clone());
                text
            }
            _ if stream => format!("lossy stream: {} % not bit-exact (or an equalizer, normalization, crossfade)", crate::number(share * 100.0)),
            _ => {
                let (first, _) = off_span.unwrap_or_default();
                format!("{off_grid} samples altered at {}", crate::timestamp(first, rate))
            }
        };
        report.issues.insert(0, analyze::Issue { severity: Severity::Altered, text });
        report.verdict = report.verdict.max(Severity::Altered);
    }
    let unproven_end = PROVES_END && !end_proven;
    if unproven_end {
        let text = "end not verified: no pause after the song";
        report.issues.insert(0, analyze::Issue { severity: Severity::Altered, text: text.into() });
        report.verdict = report.verdict.max(Severity::Altered);
    }
    let lossy = lossy.or(report.lossy_source.then_some(Lossy::Source));
    Checked { audio, report, exact, unproven_end, faded, lossy }
}

fn file_size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

fn saved(track: Track, file: Option<PathBuf>, size: u64, checked: Checked, echolot: bool) -> Update {
    let Checked { audio, mut report, exact, faded, lossy, .. } = checked;
    // The row shows the first one.
    report.issues.sort_by_key(|i| std::cmp::Reverse(i.severity));
    Update::Saved {
        track,
        file: file.map(|f| f.display().to_string()),
        size,
        verdict: report.verdict,
        master: report.master,
        exact,
        faded: faded.is_some(),
        lossy,
        issues: report.issues.into_iter().map(|i| i.text).collect(),
        echolot,
        frames: audio.frames() as u64,
        peak_dbfs: report.peak_dbfs.is_finite().then_some(report.peak_dbfs),
    }
}

#[allow(clippy::too_many_arguments)]
fn finish(track: Track, tags: Tags, samples: Vec<i32>, proof: Proof, rate: u32, lossless: bool, settings: &Settings, notify: &Notify) {
    if settings.skip_existing && let Some(file) = output::existing(&tags, settings) {
        return notify(Update::Exists { track, file: file.display().to_string() });
    }
    let checked = check(samples, proof, rate, lossless);
    // Lossy content in a lossless file would only pretend to be more: MP3 320 holds what arrived.
    let settings = &Settings { format: if checked.lossy.is_some() { Format::Mp3 } else { settings.format }, ..settings.clone() };
    match output::write(&checked.audio, &tags, settings) {
        Ok(path) => {
            let size = file_size(&path);
            // Labelled and saved for what it is: nothing to hide.
            let flagged = checked.report.verdict > Severity::Info && checked.lossy.is_none();
            if flagged && !settings.keep_flagged {
                let _ = std::fs::remove_file(&path);
            }
            let file = (!flagged || settings.keep_flagged).then_some(path);
            notify(saved(track, file, size, checked, false));
        }
        Err(error) => notify(Update::Failed { track, error }),
    }
}

/// Exact recordings (or exact after the fade-in) and lossy streams are handed on for upload: Echolot rates a lossy one
/// with its spectrum check, and a FLAC holds what arrived without a second lossy encode. Damaged ones go to "Not taken".
#[allow(clippy::too_many_arguments)]
fn finish_job(track: Track, tags: Tags, samples: Vec<i32>, proof: Proof, rate: u32, lossless: bool, settings: &Settings, notify: &Notify) -> Written {
    let mut checked = check(samples, proof, rate, lossless);
    let stream = checked.lossy == Some(Lossy::Stream);
    // A short altered stretch is a glitch, not the stream: played again like a damaged one.
    let glitch = lossless && !checked.exact && checked.faded.is_none() && !stream || checked.unproven_end;
    let in_master = lossless && !glitch && checked.report.verdict == Severity::Corrupt && dropouts_in_master(&track, &mut checked);
    if checked.report.verdict == Severity::Corrupt || glitch {
        let detail = checked.report.issues.iter().filter(|i| i.severity == Severity::Corrupt || glitch).map(|i| i.text.as_str()).collect::<Vec<_>>().join("; ");
        let file = settings.keep_flagged.then(|| Settings { out_dir: settings.out_dir.with_file_name(crate::recorder::NOT_TAKEN_DIR), ..settings.clone() }).and_then(|s| output::write(&checked.audio, &tags, &s).ok());
        let size = file.as_deref().map_or(0, file_size);
        notify(saved(track, file, size, checked, true));
        return Written::Corrupt { detail: format!("damaged: {detail}") };
    }
    match output::write(&checked.audio, &tags, settings) {
        Ok(path) => {
            let mut note = checked.faded.clone().or_else(|| stream.then(|| checked.report.issues[0].text.clone()));
            if in_master {
                let master = checked.report.issues.iter().filter(|i| i.text.starts_with("dropout in the master")).map(|i| i.text.as_str());
                note = Some(note.as_deref().into_iter().chain(master).collect::<Vec<_>>().join("; "));
            }
            notify(saved(track, Some(path.clone()), file_size(&path), checked, true));
            Written::Saved { file: path, note }
        }
        Err(error) => {
            notify(Update::Failed { track, error: error.clone() });
            Written::Failed { error }
        }
    }
}

static DROPOUTS: Mutex<Vec<(Track, Vec<Vec<i32>>)>> = Mutex::new(Vec::new());
const DROPOUT_CONTEXT_MS: usize = 100;

/// Seen: the same 1766-frame gap in three bit-exact takes of one song.
fn dropouts_in_master(track: &Track, checked: &mut Checked) -> bool {
    let Checked { audio, report, .. } = checked;
    let (ch, context) = (audio.channels, audio.rate as usize * DROPOUT_CONTEXT_MS / 1000);
    let around: Vec<Vec<i32>> = report.dropouts.iter().map(|&(s, e)| audio.samples[s.saturating_sub(context) * ch..(e + context).min(audio.frames()) * ch].to_vec()).collect();
    let mut seen = DROPOUTS.lock().unwrap();
    let before = seen.iter().position(|(t, _)| t == track).map(|i| seen.swap_remove(i).1);
    let only_dropouts = report.issues.iter().filter(|i| i.severity == Severity::Corrupt).count() == around.len();
    if !only_dropouts || before.as_ref() != Some(&around) {
        seen.push((track.clone(), around));
        return false;
    }
    for i in report.issues.iter_mut().filter(|i| i.severity == Severity::Corrupt) {
        i.severity = Severity::Info;
        i.text = i.text.replacen("dropout", "dropout in the master", 1);
    }
    report.verdict = report.issues.iter().map(|i| i.severity).max().unwrap_or(Severity::Info);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_damaged_queue_recording_is_never_handed_on_for_upload() {
        let rate = 44_100;
        // Two seconds of loud audio on the exact 24-bit grid, with 31.5 ms of digital silence in the middle.
        let mut samples: Vec<i32> = (0..rate as usize * 2 * 2).map(|i| ((i as f64 * 0.03).sin() * 4_000_000.0) as i32 | 1).collect();
        let gap = rate as usize;
        samples[gap * 2..(gap + rate as usize * 315 / 10_000) * 2].fill(0);
        let dir = std::env::temp_dir().join(format!("mitschnitt-corrupt-{}", std::process::id()));
        let settings = Settings { out_dir: dir.join("Echolot"), format: Format::Flac, keep_flagged: true, ..Settings::default() };
        let track = Track { title: "Revolution".into(), artist: "Artist".into(), album: String::new(), duration_ms: 2_000 };
        let tags = Tags { title: track.title.clone(), artist: track.artist.clone(), ..Tags::default() };
        let notify: Notify = Arc::new(|_| {});
        let take = |samples: Vec<i32>| finish_job(track.clone(), tags.clone(), samples, (0, None, true), rate, true, &settings, &notify);
        let written = take(samples.clone());
        assert!(matches!(&written, Written::Corrupt { detail } if detail.contains("dropout")), "{written:?}");
        assert!(!dir.join("Echolot").exists(), "nothing waits for an upload");
        assert!(dir.join("Not taken").join("Artist - Revolution.flac").is_file(), "kept for a look");

        let mut longer = samples.clone();
        longer[(gap - 1) * 2..gap * 2].fill(0);
        assert!(matches!(take(longer), Written::Corrupt { .. }));
        assert!(matches!(take(samples.clone()), Written::Corrupt { .. }));
        let written = take(samples);
        assert!(matches!(&written, Written::Saved { note: Some(n), .. } if n.starts_with("dropout in the master")), "{written:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_short_altered_stretch_is_a_glitch_and_a_lossy_stream_is_handed_on() {
        let rate = 44_100;
        let samples: Vec<i32> = (0..rate as usize * 2 * 2).map(|i| ((i as f64 * 0.03).sin() * 4_000_000.0) as i32 | 1).collect();
        let dir = std::env::temp_dir().join(format!("mitschnitt-glitch-{}", std::process::id()));
        let settings = Settings { out_dir: dir.join("Echolot"), format: Format::Flac, keep_flagged: false, ..Settings::default() };
        let track = Track { title: "Song".into(), artist: "Artist".into(), album: String::new(), duration_ms: 2_000 };
        let notify: Notify = Arc::new(|_| {});
        // Five samples a second in: not the player's fade-in, not a lossy stream.
        let glitch = finish_job(track.clone(), Tags::default(), samples.clone(), (5, Some((44_100, 44_104)), true), rate, true, &settings, &notify);
        assert!(matches!(&glitch, Written::Corrupt { detail } if detail.contains("0:01.000")), "{glitch:?}");
        let lossy = finish_job(track, Tags::default(), samples, (60_000, Some((0, 88_199)), true), rate, true, &settings, &notify);
        assert!(matches!(&lossy, Written::Saved { note: Some(n), .. } if n.starts_with("lossy stream")), "{lossy:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_lossy_stream_is_kept_as_mp3_not_as_a_lossless_file() {
        let rate = 44_100;
        let samples: Vec<i32> = (0..rate as usize * 2 * 2).map(|i| ((i as f64 * 0.03).sin() * 4_000_000.0) as i32 | 1).collect();
        let dir = std::env::temp_dir().join(format!("mitschnitt-lossy-{}", std::process::id()));
        let settings = Settings { out_dir: dir.clone(), format: Format::Flac, keep_flagged: false, skip_existing: false, ..Settings::default() };
        let track = Track { title: "Song".into(), artist: "Artist".into(), album: String::new(), duration_ms: 2_000 };
        let tags = Tags { title: track.title.clone(), artist: track.artist.clone(), ..Tags::default() };
        let saved = Arc::new(Mutex::new(None));
        let sink = saved.clone();
        let notify: Notify = Arc::new(move |u| {
            if let Update::Saved { file, lossy, .. } = u {
                *sink.lock().unwrap() = Some((file, lossy));
            }
        });
        finish(track, tags, samples, (60_000, Some((0, 88_199)), true), rate, true, &settings, &notify);
        let (file, lossy) = saved.lock().unwrap().take().expect("saved");
        assert_eq!(lossy, Some(Lossy::Stream));
        assert!(file.is_some_and(|f| f.ends_with(".mp3")), "kept despite keep_flagged = false, as MP3");
        let _ = std::fs::remove_dir_all(dir);
    }
}

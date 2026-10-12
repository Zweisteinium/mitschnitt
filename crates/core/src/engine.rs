//! Passive recording engine: capture + media + volume wired to the [`Passive`] state machine.
//!
//! Threads: the capture thread sends packets over a channel; the engine thread owns every platform
//! object (media session, volume controls; COM/WinRT on Windows), feeds the state machine and the monitor and polls media
//! 10x per second; each finished track is written and checked on its own short-lived worker thread.
//!
//! With a [`Queue`] attached, the engine also drives the player: a [`Runner`] on the engine thread starts each
//! requested song, and only those songs are written (as FLAC, tagged with their Spotify id and ISRC).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::analyze::{self, Master, Report, Severity};
use crate::audio::Audio;
use crate::capture::{Capture, CaptureEvent, Packet};
use crate::devices::{self, default_endpoint};
use crate::flac::Tags;
use crate::guard::Guard;
use crate::media::{self, Control, Extra, Watcher};
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
const VOLUME_POLL: Duration = Duration::from_millis(500);
/// How often to look for the player's media session while it has none (queue mode: the player may still be starting).
const FIND_SESSION: Duration = Duration::from_secs(1);

pub struct Config {
    pub profile: Profile,
    pub settings: Settings,
    /// Diagnostics: write the raw capture and a timeline of player events into this folder.
    pub trace: Option<PathBuf>,
    /// Drive the player through these songs from the start (the player needs no media session yet).
    pub queue: Option<Queue>,
}

/// Songs to play and record one after another (see [`crate::runner`]).
pub struct Queue {
    /// The next song; taken whenever the runner is ready.
    pub jobs: Receiver<Job>,
    /// Every finished song with its outcome.
    pub done: Sender<(Job, Outcome)>,
    /// What the runner does right now.
    pub status: Arc<Mutex<QueueStatus>>,
    /// Where the recordings go.
    pub out_dir: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct QueueStatus {
    /// The song being worked on.
    pub job: Option<Job>,
    /// It is being recorded right now.
    pub recording: bool,
    pub recorded_ms: u64,
    /// The player is playing (anything).
    pub playing: bool,
}

/// Raw capture (`raw.f32`, interleaved float as captured, silent packets as zeros) and a timeline
/// (`events.tsv`: frame, kind, details) to study how a player behaves at starts, switches and pauses.
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
    /// Complete, but a file with its name exists already (`skip_existing`).
    Exists { track: Track, file: String },
    /// Written and checked. `file` is `None` when a flagged file was removed (`keep_flagged` off).
    /// `exact`: every sample arrived as an exact 24-bit value - an untouched lossless source. `echolot`: recorded for
    /// Echolot's queue.
    Saved { track: Track, file: Option<String>, size: u64, verdict: Severity, master: Master, exact: bool, issues: Vec<String>, echolot: bool },
    Failed { track: Track, error: String },
    /// A media key was swallowed to protect the recording (`count` since listening started).
    KeyBlocked { count: u32 },
    /// The monitor muted itself because the default output changed (or unmuted again).
    MonitorMuted { muted: bool },
    /// The monitor's volume (its Windows mixer entry), also when changed in the mixer.
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
    /// The file of the queue's current song was written (or not).
    Written(Written),
    Stop,
}

/// A running engine; stops (and restores volumes) when dropped.
pub struct Engine {
    tx: Sender<Msg>,
    monitor_volume: Option<Arc<AtomicU32>>,
    monitor_state: Option<Arc<MonitorState>>,
    thread: Option<JoinHandle<()>>,
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
                        let monitor = session.monitor.as_ref().map(|m| (m.volume_handle(), m.state()));
                        let _ = ready_tx.send(Ok((session.info.clone(), monitor)));
                        if let Some(queue) = queue {
                            session.attach(queue);
                        }
                        session.run(&rx, &config.settings, &notify);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let (info, monitor) = ready_rx.recv().map_err(|e| e.to_string())??;
        let (monitor_volume, monitor_state) = monitor.unzip();
        Ok((Engine { tx, monitor_volume, monitor_state, thread: Some(thread) }, info))
    }

    /// Finish the track being recorded, then stop (immediately when none is).
    pub fn stop_after_track(&self) {
        let _ = self.tx.send(Msg::StopAfterTrack);
    }

    /// Start driving the player through `queue` (a listening engine keeps running underneath). Gives the queue
    /// back when this engine has stopped meanwhile (player closed, capture error).
    pub fn attach(&self, queue: Queue) -> Result<(), Queue> {
        match self.tx.send(Msg::Attach(queue)) {
            Ok(()) => Ok(()),
            Err(mpsc::SendError(Msg::Attach(queue))) => Err(queue),
            Err(_) => unreachable!("sent an Attach"),
        }
    }

    /// Stop driving the player; a song being worked on counts as stalled.
    pub fn detach(&self) {
        let _ = self.tx.send(Msg::Detach);
    }

    /// Unmute a monitor that muted itself on an output change; the current output becomes its device.
    pub fn unmute_monitor(&self) {
        if let Some(s) = &self.monitor_state {
            s.unmute();
        }
    }

    /// Monitoring volume, 0..1 (no-op when monitoring is off).
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

/// What the engine changed and must put back. Persisted while listening, so a crash or a killed process
/// is repaired on the next start ([`recover`]). Controls are looked up afresh when applied: a restarted
/// app has new sessions, and stale controls would silently miss them.
#[derive(Serialize, Deserialize)]
struct Restore {
    app: String,
    /// The app's own volume, once it has an audio stream.
    app_volume: Option<f32>,
    /// "Listen to this device" was audible and got muted by us.
    unmute_listen: bool,
    /// The forced sample rate to go back to.
    rate: Option<u32>,
}

impl Restore {
    fn path() -> PathBuf {
        let base = dirs::state_dir().or_else(dirs::data_local_dir).unwrap_or_else(std::env::temp_dir);
        base.join(crate::settings::APP_DIR).join("restore.json")
    }

    fn save(&self) {
        let path = Self::path();
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
        let _ = std::fs::write(&path, serde_json::to_string(self).unwrap_or_default());
    }

    fn load() -> Option<Restore> {
        serde_json::from_str(&std::fs::read_to_string(Self::path()).ok()?).ok()
    }

    /// Put everything back and forget the record.
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
        let _ = std::fs::remove_file(Self::path());
    }
}

/// Undo changes left behind by a previous run that didn't end cleanly (crash, killed process).
pub fn recover() {
    if let Some(restore) = Restore::load() {
        init_thread();
        restore.apply();
    }
}

/// Where an app without an audio stream yet will play (a queue starts the player itself): into the cable, at the
/// app's rate. Capturing at another rate would resample every song.
fn expected_endpoint(profile: &Profile) -> Result<devices::Endpoint, String> {
    let default = default_endpoint().map_err(|e| e.to_string())?;
    if profile.rate == 0 {
        return Ok(default);
    }
    let cable = devices::render_endpoints().ok().into_iter().flatten().find(|e| e.name.contains("CABLE Input"));
    Ok(cable.unwrap_or(devices::Endpoint { sample_rate: profile.rate, ..default }))
}

/// A queue being worked through, with its runner.
struct QueueRun {
    queue: Queue,
    runner: Runner,
    /// The monitor is silenced for the queue (it plays songs nobody chose to listen to): the level to go back to.
    quiet: Option<f32>,
}

/// Everything that lives on the engine thread.
struct Session {
    info: SourceInfo,
    profile: Profile,
    root_pid: u32,
    /// The player's media session; in queue mode it may only appear once the first song plays.
    watcher: Option<Watcher>,
    find_at: Instant,
    /// Album artist, track number and cover, read when the current recording started.
    extra: Option<(Track, Extra)>,
    trace: Option<Trace>,
    /// Protects recordings from media keys and headset commands.
    guard: Option<Guard>,
    restore: Restore,
    passive: Passive,
    monitor: Option<Monitor>,
    queue: Option<QueueRun>,
    /// The output devices (see [`devices::layout`]): a change can glitch every stream for a moment.
    layout: String,
    /// For worker threads to report back.
    tx: Sender<Msg>,
    _capture: Capture,
}

impl Session {
    fn open(config: &Config, queued: bool, packets: Sender<Msg>) -> Result<Session, String> {
        recover();
        let profile = &config.profile;
        let session = media::find(profile).map_err(|e| e.to_string())?;
        if session.is_none() && !queued {
            return Err(format!("{} shows no playback yet - start playback in the app first.", profile.name));
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
        let mut restore = Restore { app: profile.exe.to_owned(), app_volume, unmute_listen, rate: None };
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
            sessions::set_mute(&listen, true); // our monitor replaces it; avoid hearing everything twice
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
        let monitor = monitoring.then(|| Monitor::start(endpoint.sample_rate, CHANNELS, GAIN)).flatten();
        Ok(Session {
            info: SourceInfo { app: profile.name.to_owned(), device: endpoint.name, rate: endpoint.sample_rate },
            profile: profile.clone(),
            root_pid: root.pid,
            watcher: session.map(Watcher::new),
            find_at: Instant::now(),
            extra: None,
            // Best effort: without the guard, listening still works (media keys just aren't blocked).
            guard: config.settings.protect_media_keys.then(Guard::start).and_then(Result::ok),
            trace: match &config.trace {
                Some(dir) => Some(Trace::open(dir).map_err(|e| format!("trace: {e}"))?),
                None => None,
            },
            restore,
            passive: Passive::new(endpoint.sample_rate, CHANNELS as usize, GAIN),
            monitor,
            queue: None,
            layout: devices::layout(),
            tx,
            _capture: capture,
        })
    }

    fn attach(&mut self, queue: Queue) {
        self.detach(Outcome::Stalled { detail: "replaced by another queue".into() });
        self.queue = Some(QueueRun { queue, runner: Runner::new(Instant::now()), quiet: None });
    }

    /// Stop driving the player: a song being worked on ends with `outcome`, the monitor gets its level back.
    fn detach(&mut self, outcome: Outcome) {
        let Some(mut run) = self.queue.take() else { return };
        let actions = run.runner.abort(Instant::now(), outcome);
        self.act(&actions, run.runner.job());
        for done in run.runner.take_done() {
            let _ = run.queue.done.send(done);
        }
        if let (Some(level), Some(monitor)) = (run.quiet, &self.monitor) {
            // Moved by the user during the queue (slider or mixer): that level stays.
            let handle = monitor.volume_handle();
            if f32::from_bits(handle.load(Ordering::Relaxed)) == 0.0 {
                handle.store(level.to_bits(), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(50)); // let the monitor apply it before it may stop
            }
        }
        if let Ok(mut status) = run.queue.status.lock() {
            *status = QueueStatus::default();
        }
    }

    /// Carry out the runner's actions on the player.
    fn act(&self, actions: &[Action], job: Option<&Job>) {
        for action in actions {
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
                eprintln!("runner: {action:?}: {e}");
            }
        }
    }

    fn run(mut self, rx: &mpsc::Receiver<Msg>, settings: &Settings, notify: &Notify) {
        // Keep the PC awake while listening (the display may still turn off); released on this thread below.
        let awake = KeepAwake::start();
        let (mut next_media, mut next_volume) = (Instant::now(), Instant::now() + VOLUME_POLL);
        let (mut error, mut stop_after) = (None, false);
        let (mut blocked, mut muted, mut volume) = (0, false, f32::NAN);
        loop {
            // A queue's song that is being written still needs the engine for its result.
            if stop_after && !self.passive.recording() && !self.queue.as_ref().is_some_and(|q| q.runner.writing()) {
                break;
            }
            if let Some(guard) = &mut self.guard {
                guard.update(self.passive.recording(), self.extra.as_ref());
            }
            let timeout = next_media.saturating_duration_since(Instant::now());
            let events = match rx.recv_timeout(timeout) {
                Ok(Msg::Packet(p)) => {
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
            if now >= next_media {
                next_media = now + MEDIA_POLL;
                if self.watcher.is_none() && now >= self.find_at {
                    self.find_at = now + FIND_SESSION;
                    self.watcher = media::find(&self.profile).ok().flatten().map(Watcher::new);
                }
                let snapshot = self.watcher.as_mut().map(|w| w.snapshot().map_err(|e| e.to_string()));
                match snapshot {
                    Some(Ok(mut media)) => {
                        // An ad is not a track: as far as recording goes, nothing is playing.
                        media.track = media.track.filter(|t| !(self.profile.is_ad)(t));
                        if let Some(t) = &mut self.trace {
                            t.media(&media);
                        }
                        let events = self.passive.media(media.clone());
                        self.dispatch(events, settings, notify);
                        self.drive(now, &media);
                    }
                    Some(Err(e)) => {
                        error = Some(format!("media session lost: {e}"));
                        break;
                    }
                    None => self.drive(now, &crate::passive::Media::default()),
                }
                if now >= next_volume {
                    next_volume = now + VOLUME_POLL;
                    if !sessions::is_running(self.root_pid) {
                        error = Some(format!("{} was closed.", self.info.app));
                        break;
                    }
                    // Windows recreates "Listen to this device" when the outputs change, unmuted: while the monitor
                    // plays, every one of them stays muted (and is unmuted again at the end).
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
                    // Devices coming or going can cut the player's audio for a moment: a queue's song is played again.
                    let layout = devices::layout();
                    if layout != self.layout {
                        self.layout = layout;
                        if self.queue.is_some() {
                            let events = self.passive.interrupt(Reason::DeviceChanged);
                            self.dispatch(events, settings, notify);
                        }
                    }
                    let controls = sessions::app_volumes_by_name(self.profile.exe);
                    let volumes = sessions::get(&controls);
                    if volumes.iter().any(|v| (v - GAIN).abs() > 1e-4) {
                        if self.restore.app_volume.is_none() {
                            self.restore.app_volume = volumes.into_iter().reduce(f32::max);
                            self.restore.save();
                        }
                        // Someone moved the app's volume (or a new stream appeared): that recording is unusable.
                        // Put it back so the following tracks are exact again (use the monitor volume instead).
                        let events = self.passive.volume_changed();
                        self.dispatch(events, settings, notify);
                        sessions::set(&controls, GAIN);
                    }
                }
                notify(Update::Status { status: self.passive.status() });
                if let Some(count) = self.guard.as_ref().map(Guard::blocked).filter(|c| *c != blocked) {
                    blocked = count;
                    notify(Update::KeyBlocked { count });
                }
                if let Some(now) = self.monitor.as_ref().map(|m| m.state().muted.load(Ordering::Relaxed)).filter(|m| *m != muted) {
                    muted = now;
                    notify(Update::MonitorMuted { muted });
                }
                if let Some(now) = self.monitor.as_ref().map(|m| f32::from_bits(m.volume_handle().load(Ordering::Relaxed))).filter(|v| !v.is_nan() && *v != volume) {
                    volume = now;
                    notify(Update::MonitorVolume { volume });
                }
            }
        }
        let events = self.passive.stop();
        self.dispatch(events, settings, notify);
        // Stopped by the user (Stop, closing the app): his take-over. A failure: worth another try.
        self.detach(match &error {
            Some(e) => Outcome::Stalled { detail: e.clone() },
            None => Outcome::TookOver { detail: "listening was stopped".into() },
        });
        self.guard = None;
        self.monitor = None;
        self.restore.apply();
        drop(awake);
        notify(Update::Stopped { error });
    }

    /// Queue mode, once per media snapshot: let the runner act, hand it the next song, report what finished.
    fn drive(&mut self, now: Instant, media: &crate::passive::Media) {
        let Some(run) = &mut self.queue else { return };
        // Silence the monitor while the queue runs, once its level is known.
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
        if run.runner.ready(now)
            && let Ok(job) = run.queue.jobs.try_recv()
        {
            actions.extend(run.runner.assign(job, now, media));
        }
        let job = run.runner.job().cloned();
        let done = run.runner.take_done();
        let status = QueueStatus { job: job.clone(), recording: run.runner.recording(), recorded_ms: 0, playing: media.playing };
        for d in done {
            let _ = run.queue.done.send(d);
        }
        let recorded_ms = self.passive.recorded_ms();
        if let Ok(mut s) = run.queue.status.lock() {
            *s = QueueStatus { recorded_ms, ..status };
        }
        self.act(&actions, job.as_ref());
    }

    fn dispatch(&mut self, events: Vec<Event>, settings: &Settings, notify: &Notify) {
        for event in events {
            if let Some(t) = &mut self.trace {
                t.note(&match &event {
                    Event::Started(track) => format!("started\t{}", track.title),
                    Event::Dropped { track, reason } => format!("dropped\t{}\t{reason:?}", track.title),
                    Event::Completed { track, off_grid, off_span, .. } => format!("completed\t{}\toff_grid={off_grid}\toff_span={off_span:?}", track.title),
                });
            }
            // Queue mode: the runner sees every event first; only its own songs are reported and written.
            let mut target = true;
            if let Some(run) = &mut self.queue {
                let now = Instant::now();
                let actions = run.runner.event(now, &event);
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
                Event::Completed { track, samples, off_grid, off_span } => {
                    let extra = self.extra.take().filter(|(t, _)| *t == track).map(|(_, e)| e).unwrap_or_default();
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
                        // Leftovers of the runner's own starts and stops (the album playing on) are not kept.
                        Some(_) if !target => {}
                        Some(run) => {
                            let job = run.runner.job().cloned();
                            if let Some(job) = &job {
                                tags.extra = vec![("SPOTIFY_TRACK_ID".into(), job.track_id().into()), ("ISRC".into(), job.isrc.clone())];
                            }
                            // Always a FLAC that is kept, whatever the listening settings say.
                            let settings = Settings { out_dir: run.queue.out_dir.clone(), format: Format::Flac, keep_flagged: true, skip_existing: false, ..settings.clone() };
                            let (tx, done) = (self.tx.clone(), run.queue.done.clone());
                            std::thread::spawn(move || {
                                let written = finish_job(track, tags, samples, (off_grid, off_span), rate, lossless, &settings, &notify);
                                // The engine stopped meanwhile: report the result directly.
                                if let (Err(mpsc::SendError(Msg::Written(written))), Some(job)) = (tx.send(Msg::Written(written)), job) {
                                    let _ = done.send((job, written.into()));
                                }
                            });
                        }
                        None => {
                            let settings = settings.clone();
                            std::thread::spawn(move || finish(track, tags, samples, (off_grid, off_span), rate, lossless, &settings, &notify));
                        }
                    }
                }
            }
        }
    }
}

/// A finished recording, checked: the audio at 24 bit, the forensic report and whether the source was exact.
struct Checked {
    audio: Audio,
    report: Report,
    exact: bool,
    /// Only the player's fade-in at the start is not original: what to tell about it.
    faded: Option<String>,
    /// Off the grid in more than a short stretch: the stream wasn't lossless (or was processed).
    lossy: bool,
}

/// Share of the audio that may be off the grid in one short stretch of an otherwise exact recording: that is a glitch
/// (worth another try), not a lossy stream.
const GLITCH_SHARE: f64 = 0.001;

fn check(samples: Vec<i32>, (off_grid, off_span): (usize, Option<(usize, usize)>), rate: u32, lossless: bool) -> Checked {
    let audio = Audio { rate, channels: CHANNELS as usize, bits: 24, samples };
    let exact = off_grid == 0;
    let mut report = analyze::analyze_with(&audio, exact);
    let mut faded = None;
    let share = off_grid as f64 / audio.frames().max(1) as f64;
    let lossy = lossless && !exact && share > GLITCH_SHARE;
    if !exact && lossless {
        let ms = |frames: usize| frames as f64 * 1000.0 / rate as f64;
        let text = match off_span {
            // Players fade in when the user starts a track (Spotify: ~10 ms); everything after is untouched.
            Some((first, last)) if ms(last) < 250.0 && ms(first) < 5.0 => {
                report.issues.retain(|i| !i.text.starts_with("fade-in at the start"));
                let text = format!("faded in by the player: the first {:.1} ms are not original, everything after is bit-exact", ms(last + 1));
                faded = Some(text.clone());
                text
            }
            _ if lossy => format!("{:.1} % of the audio did not arrive as exact 24-bit values: lossy stream or processing (normalization, equalizer, crossfade)", share * 100.0),
            _ => {
                let at = |frame: usize| crate::timestamp(frame, rate);
                let (first, last) = off_span.unwrap_or_default();
                format!("{off_grid} samples between {} and {} are not original, everything else is bit-exact", at(first), at(last))
            }
        };
        report.issues.insert(0, analyze::Issue { severity: Severity::Altered, text });
        report.verdict = report.verdict.max(Severity::Altered);
    }
    Checked { audio, report, exact, faded, lossy }
}

fn file_size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or_default()
}

/// Write, verify and check one finished track.
#[allow(clippy::too_many_arguments)]
fn finish(track: Track, tags: Tags, samples: Vec<i32>, grid: (usize, Option<(usize, usize)>), rate: u32, lossless: bool, settings: &Settings, notify: &Notify) {
    if settings.skip_existing && let Some(file) = output::existing(&tags, settings) {
        return notify(Update::Exists { track, file: file.display().to_string() });
    }
    let Checked { audio, report, exact, .. } = check(samples, grid, rate, lossless);
    match output::write(&audio, &tags, settings) {
        Ok(path) => {
            let flagged = report.verdict > Severity::Info;
            let size = file_size(&path);
            let file = if flagged && !settings.keep_flagged {
                let _ = std::fs::remove_file(&path);
                None
            } else {
                Some(path.display().to_string())
            };
            notify(Update::Saved {
                file,
                size,
                verdict: report.verdict,
                master: report.master,
                exact,
                issues: report.issues.into_iter().map(|i| i.text).collect(),
                echolot: false,
                track,
            });
        }
        Err(error) => notify(Update::Failed { track, error }),
    }
}

/// Write one song of a queue. Only an undamaged, lossless recording is handed on for the upload: exact, or exact after
/// the player's fade-in. A damaged one goes to "Not taken" (when flagged files are kept) and is never uploaded.
#[allow(clippy::too_many_arguments)]
fn finish_job(track: Track, tags: Tags, samples: Vec<i32>, grid: (usize, Option<(usize, usize)>), rate: u32, lossless: bool, settings: &Settings, notify: &Notify) -> Written {
    let Checked { audio, report, exact, faded, lossy } = check(samples, grid, rate, lossless);
    if lossy {
        let detail = report.issues.first().map(|i| i.text.clone()).unwrap_or_default();
        notify(Update::Failed { track, error: format!("not lossless, not kept: {detail}") });
        return Written::Lossy { detail };
    }
    // Damaged, or altered in a short stretch (a glitch, not the stream): never uploaded, played again.
    let glitch = lossless && !exact && faded.is_none();
    if report.verdict == Severity::Corrupt || glitch {
        let detail = report.issues.iter().filter(|i| i.severity == Severity::Corrupt || glitch).map(|i| i.text.as_str()).collect::<Vec<_>>().join("; ");
        let file = settings.keep_flagged.then(|| Settings { out_dir: settings.out_dir.with_file_name(crate::recorder::NOT_TAKEN_DIR), ..settings.clone() }).and_then(|s| output::write(&audio, &tags, &s).ok());
        notify(Update::Saved {
            size: file.as_deref().map(file_size).unwrap_or_default(),
            file: file.map(|p| p.display().to_string()),
            verdict: report.verdict,
            master: report.master,
            exact,
            issues: report.issues.into_iter().map(|i| i.text).collect(),
            echolot: true,
            track,
        });
        return Written::Corrupt { detail: format!("damaged, not uploaded: {detail}") };
    }
    match output::write(&audio, &tags, settings) {
        Ok(path) => {
            notify(Update::Saved {
                file: Some(path.display().to_string()),
                size: file_size(&path),
                verdict: report.verdict,
                master: report.master,
                exact,
                issues: report.issues.into_iter().map(|i| i.text).collect(),
                echolot: true,
                track,
            });
            Written::Saved { file: path, note: faded }
        }
        Err(error) => {
            notify(Update::Failed { track, error: error.clone() });
            Written::Failed { error }
        }
    }
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
        let written = finish_job(track, tags, samples, (0, None), rate, true, &settings, &notify);
        assert!(matches!(&written, Written::Corrupt { detail } if detail.contains("dropout")), "{written:?}");
        assert!(!dir.join("Echolot").exists(), "nothing waits for an upload");
        assert!(dir.join("Not taken").join("Artist - Revolution.flac").is_file(), "kept for a look");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_short_altered_stretch_is_a_glitch_and_a_mostly_altered_stream_is_lossy() {
        let rate = 44_100;
        let samples: Vec<i32> = (0..rate as usize * 2 * 2).map(|i| ((i as f64 * 0.03).sin() * 4_000_000.0) as i32 | 1).collect();
        let dir = std::env::temp_dir().join(format!("mitschnitt-glitch-{}", std::process::id()));
        let settings = Settings { out_dir: dir.join("Echolot"), format: Format::Flac, keep_flagged: false, ..Settings::default() };
        let track = Track { title: "Song".into(), artist: "Artist".into(), album: String::new(), duration_ms: 2_000 };
        let notify: Notify = Arc::new(|_| {});
        // Five samples a second in: not the player's fade-in, not a lossy stream.
        let glitch = finish_job(track.clone(), Tags::default(), samples.clone(), (5, Some((44_100, 44_104))), rate, true, &settings, &notify);
        assert!(matches!(&glitch, Written::Corrupt { detail } if detail.contains("0:01.000")), "{glitch:?}");
        let lossy = finish_job(track, Tags::default(), samples, (60_000, Some((0, 88_199))), rate, true, &settings, &notify);
        assert!(matches!(lossy, Written::Lossy { .. }));
        assert!(!dir.join("Echolot").exists(), "nothing waits for an upload");
        let _ = std::fs::remove_dir_all(dir);
    }
}

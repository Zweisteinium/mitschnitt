//! Passive recording engine: capture + media + volume wired to the [`Passive`] state machine.
//!
//! Threads: the capture thread sends packets over a channel; the engine thread owns every platform
//! object (media session, volume controls; COM/WinRT on Windows), feeds the state machine and the monitor and polls media
//! 10x per second; each finished track is written and checked on its own short-lived worker thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::analyze::{self, Master, Severity};
use crate::audio::Audio;
use crate::capture::{Capture, CaptureEvent, Packet};
use crate::devices::{self, default_endpoint};
use crate::flac::Tags;
use crate::guard::Guard;
use crate::media::{self, Extra, Watcher};
use crate::monitor::{Monitor, MonitorState};
use crate::output;
use crate::passive::{Event, Passive, Reason, Status, Track};
use crate::power::KeepAwake;
use crate::profile::Profile;
use crate::settings::Settings;
use crate::{init_thread, sessions};

pub use crate::platform::os::GAIN;
const CHANNELS: u16 = 2;
const MEDIA_POLL: Duration = Duration::from_millis(100);
const VOLUME_POLL: Duration = Duration::from_millis(500);

pub struct Config {
    pub profile: Profile,
    pub settings: Settings,
    /// Diagnostics: write the raw capture and a timeline of player events into this folder.
    pub trace: Option<PathBuf>,
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
    /// `exact`: every sample arrived as an exact 24-bit value - an untouched lossless source.
    Saved { track: Track, file: Option<String>, verdict: Severity, master: Master, exact: bool, issues: Vec<String> },
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
                match Session::open(&config, packets) {
                    Ok(session) => {
                        let monitor = session.monitor.as_ref().map(|m| (m.volume_handle(), m.state()));
                        let _ = ready_tx.send(Ok((session.info.clone(), monitor)));
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

/// Everything that lives on the engine thread.
struct Session {
    info: SourceInfo,
    profile: Profile,
    root_pid: u32,
    watcher: Watcher,
    /// Album artist, track number and cover, read when the current recording started.
    extra: Option<(Track, Extra)>,
    trace: Option<Trace>,
    /// Protects recordings from media keys and headset commands.
    guard: Option<Guard>,
    restore: Restore,
    passive: Passive,
    monitor: Option<Monitor>,
    _capture: Capture,
}

impl Session {
    fn open(config: &Config, packets: Sender<Msg>) -> Result<Session, String> {
        recover();
        let profile = &config.profile;
        let session = media::find(profile)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{} shows no playback yet - start playback in the app first.", profile.name))?;
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
        let mut endpoint = sessions::app_endpoint(&tree).map_or_else(|| default_endpoint().map_err(|e| e.to_string()), Ok)?;
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
            watcher: Watcher::new(session),
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
            _capture: capture,
        })
    }

    fn run(mut self, rx: &mpsc::Receiver<Msg>, settings: &Settings, notify: &Notify) {
        // Keep the PC awake while listening (the display may still turn off); released on this thread below.
        let awake = KeepAwake::start();
        let (mut next_media, mut next_volume) = (Instant::now(), Instant::now() + VOLUME_POLL);
        let (mut error, mut stop_after) = (None, false);
        let (mut blocked, mut muted, mut volume) = (0, false, f32::NAN);
        loop {
            if stop_after && !self.passive.recording() {
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
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => Vec::new(),
            };
            self.dispatch(events, settings, notify);

            let now = Instant::now();
            if now >= next_media {
                next_media = now + MEDIA_POLL;
                match self.watcher.snapshot() {
                    Ok(mut media) => {
                        // An ad is not a track: as far as recording goes, nothing is playing.
                        media.track = media.track.filter(|t| !(self.profile.is_ad)(t));
                        if let Some(t) = &mut self.trace {
                            t.media(&media);
                        }
                        let events = self.passive.media(media);
                        self.dispatch(events, settings, notify);
                    }
                    Err(e) => {
                        error = Some(format!("media session lost: {e}"));
                        break;
                    }
                }
                if now >= next_volume {
                    next_volume = now + VOLUME_POLL;
                    if !sessions::is_running(self.root_pid) {
                        error = Some(format!("{} was closed.", self.info.app));
                        break;
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
        self.guard = None;
        self.monitor = None;
        self.restore.apply();
        drop(awake);
        notify(Update::Stopped { error });
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
            match event {
                Event::Started(track) => {
                    self.extra = Some((track.clone(), self.watcher.extra()));
                    notify(Update::Started { track });
                }
                Event::Dropped { track, reason } => notify(Update::Dropped { track, reason, message: reason.message().to_owned() }),
                Event::Completed { track, samples, off_grid, off_span } => {
                    let extra = self.extra.take().filter(|(t, _)| *t == track).map(|(_, e)| e).unwrap_or_default();
                    let tags = Tags {
                        title: track.title.clone(),
                        artist: track.artist.clone(),
                        album: track.album.clone(),
                        album_artist: extra.album_artist,
                        track_number: extra.track_number,
                        cover: extra.cover,
                    };
                    let (rate, lossless, settings, notify) = (self.info.rate, self.profile.lossless, settings.clone(), notify.clone());
                    std::thread::spawn(move || finish(track, tags, samples, (off_grid, off_span), rate, lossless, &settings, &notify));
                }
            }
        }
    }
}

/// Write, verify and check one finished track.
#[allow(clippy::too_many_arguments)]
fn finish(track: Track, tags: Tags, samples: Vec<i32>, (off_grid, off_span): (usize, Option<(usize, usize)>), rate: u32, lossless: bool, settings: &Settings, notify: &Notify) {
    if settings.skip_existing && let Some(file) = output::existing(&tags, settings) {
        return notify(Update::Exists { track, file: file.display().to_string() });
    }
    let audio = Audio { rate, channels: CHANNELS as usize, bits: 24, samples };
    let exact = off_grid == 0;
    let mut report = analyze::analyze_with(&audio, exact);
    if !exact && lossless {
        let ms = |frames: usize| frames as f64 * 1000.0 / rate as f64;
        let text = match off_span {
            // Players fade in when the user starts a track (Spotify: ~10 ms); everything after is untouched.
            Some((first, last)) if ms(last) < 250.0 && ms(first) < 5.0 => {
                report.issues.retain(|i| !i.text.starts_with("fade-in at the start"));
                format!("faded in by the player: the first {:.1} ms are not original, everything after is bit-exact", ms(last + 1))
            }
            _ => {
                let share = off_grid as f64 * 100.0 / audio.frames().max(1) as f64;
                format!("{share:.1} % of the audio did not arrive as exact 24-bit values: lossy stream or processing (normalization, equalizer, crossfade)")
            }
        };
        report.issues.insert(0, analyze::Issue { severity: Severity::Altered, text });
        report.verdict = report.verdict.max(Severity::Altered);
    }
    match output::write(&audio, &tags, settings) {
        Ok(path) => {
            let flagged = report.verdict > Severity::Info;
            let file = if flagged && !settings.keep_flagged {
                let _ = std::fs::remove_file(&path);
                None
            } else {
                Some(path.display().to_string())
            };
            notify(Update::Saved {
                file,
                verdict: report.verdict,
                master: report.master,
                exact,
                issues: report.issues.into_iter().map(|i| i.text).collect(),
                track,
            });
        }
        Err(error) => notify(Update::Failed { track, error }),
    }
}

//! Working through a queue of songs: Echolot's (connected device) or an exported list (offline fallback).
//!
//! A connected device keeps a long-poll loop with Echolot: it reports what it does (idle, busy, recording, setup
//! problem) and gets one song at a time. For a song it makes sure the player runs, attaches a [`Queue`] to the
//! engine (started for it, or the one already listening) and reports every outcome. Recordings are uploaded on
//! their own thread with retries, so the next song can start meanwhile. When the session isn't running any more,
//! the engine is let go again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::echolot::{self, Client, DeviceState, Error, Export, Item, SessionInfo, Spotify};
use crate::engine::{Queue, QueueStatus};
use crate::profile::Profile;
use crate::runner::{Job, Outcome};
use crate::settings::Settings;
use crate::setup::{self, State};
use crate::{desktop, init_thread, media, sessions};

/// Recordings waiting for Echolot's answer (and an exported list's recordings, for Echolot's upload dialog).
pub const PENDING_DIR: &str = "Echolot";
/// Recordings Echolot didn't take.
pub const NOT_TAKEN_DIR: &str = "Not taken";

const LONG_POLL_S: u32 = 25;
const HEARTBEAT: Duration = Duration::from_secs(30);
const SETUP_CHECK: Duration = Duration::from_secs(60);
const PLAYER_START: Duration = Duration::from_secs(30);
/// Upload retries: growing pauses, giving up after an hour.
const UPLOAD_RETRY_FIRST: Duration = Duration::from_secs(10);
const UPLOAD_RETRY_MAX: Duration = Duration::from_secs(300);
const UPLOAD_GIVE_UP: Duration = Duration::from_secs(3_600);

/// Where the engine lives (the app or the CLI).
pub trait Host: Send + Sync + 'static {
    /// Drive `queue`: attach it to the running engine, or start one for it.
    fn attach(&self, queue: Queue) -> Result<(), String>;
    /// Stop driving the player; stop the engine again if it was started for the queue.
    fn detach(&self);
    /// The current settings (output folder, whether to keep copies).
    fn settings(&self) -> Settings;
    fn update(&self, update: RecorderUpdate);
}

/// What the recorder shows (the card in the app).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    /// `connected`, `offline` (Echolot not reachable), `disconnected`, or `file` (an exported list runs).
    pub connection: String,
    pub user: String,
    pub host: String,
    pub device: String,
    /// What this device told Echolot it does.
    pub state: Option<DeviceState>,
    pub detail: String,
    pub session: Option<SessionInfo>,
    /// The song being worked on.
    pub current: Option<Job>,
    /// Uploads waiting or running.
    pub uploads: usize,
    pub error: Option<String>,
    /// Exported list: songs done / total.
    pub done: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecorderUpdate {
    Snapshot { snapshot: Box<Snapshot> },
    /// One song finished: Echolot's answer (`filed`, `not_better`, `rejected`) or the reason it wasn't recorded.
    Song { job: Job, result: String, detail: String },
}

/// Shared between the loop and the commands (pause/resume/stop, disconnect).
pub struct Recorder {
    pub client: Option<Client>,
    snapshot: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
}

impl Recorder {
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// End the loop (it lets the engine go).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Pause, resume or stop Echolot's session.
    pub fn session(&self, action: &str, host: &dyn Host) -> Result<(), String> {
        let client = self.client.as_ref().ok_or("not connected")?;
        let session = client.session(action).map_err(|e| e.to_string())?;
        let snapshot = {
            let mut s = self.snapshot.lock().map_err(|e| e.to_string())?;
            s.session = session;
            s.clone()
        };
        host.update(RecorderUpdate::Snapshot { snapshot: Box::new(snapshot) });
        Ok(())
    }
}

fn publish(snapshot: &Mutex<Snapshot>, host: &dyn Host, change: impl FnOnce(&mut Snapshot)) {
    let snapshot = match snapshot.lock() {
        Ok(mut s) => {
            change(&mut s);
            s.clone()
        }
        Err(_) => return,
    };
    host.update(RecorderUpdate::Snapshot { snapshot: Box::new(snapshot) });
}

/// Start the player when it isn't running (opening its scheme launches it) and wait for its process.
pub fn ensure_player(profile: &Profile) -> Result<(), String> {
    let running = || sessions::find_app(profile.exe, &sessions::processes()).is_some();
    if running() {
        return Ok(());
    }
    // Its window ends up minimized, without taking the focus.
    desktop::open_link_in_background(&format!("{}:", profile.id), profile.exe)?;
    let deadline = Instant::now() + PLAYER_START;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        if running() {
            // Let it finish starting up before songs are opened in it.
            std::thread::sleep(Duration::from_secs(3));
            return Ok(());
        }
    }
    Err(format!("{} didn't start", profile.name))
}

fn spotify_state(profile: &Profile, status: Option<&QueueStatus>) -> Spotify {
    if let Some(s) = status {
        return if s.playing { Spotify::Playing } else { Spotify::Idle };
    }
    match media::sources().ok().and_then(|s| s.into_iter().find(|s| s.id == profile.id)) {
        Some(s) if s.playing => Spotify::Playing,
        Some(s) if s.running => Spotify::Idle,
        _ => Spotify::Closed,
    }
}

/// The first red Setup check, if any (Echolot then waits: `state: setup`).
fn setup_problem(profile: &Profile) -> Option<String> {
    setup::health(profile).into_iter().find(|c| c.state == State::Bad).map(|c| format!("{}: {}", c.title, c.detail))
}

/// The engine side of a queue: what the recorder keeps while it drives the player.
struct Attached {
    jobs: Sender<Job>,
    done: Receiver<(Job, Outcome)>,
    status: Arc<Mutex<QueueStatus>>,
}

fn attach(host: &dyn Host, profile: &Profile, out_dir: &Path) -> Result<Attached, String> {
    ensure_player(profile)?;
    let (jobs, jobs_rx) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let status = Arc::new(Mutex::new(QueueStatus::default()));
    host.attach(Queue { jobs: jobs_rx, done: done_tx, status: status.clone(), out_dir: out_dir.to_owned() })?;
    Ok(Attached { jobs, done, status })
}

/// Move a file to `target` (folders created when needed), as "name (2).flac" etc. when that exists.
fn move_to(file: &Path, target: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(target.parent()?).ok()?;
    let stem = target.file_stem()?.to_string_lossy().into_owned();
    let mut path = target.to_owned();
    let mut n = 2;
    while path.exists() {
        path = target.with_file_name(format!("{stem} ({n}).flac"));
        n += 1;
    }
    // Another drive: copy, then remove.
    let moved = std::fs::rename(file, &path).is_ok() || (std::fs::copy(file, &path).is_ok() && std::fs::remove_file(file).is_ok());
    moved.then_some(path)
}

/// Keep working through Echolot's queue on this connected device until stopped or disconnected.
pub fn connect(client: Client, saved: echolot::store::Saved, profile: Profile, out_dir: PathBuf, host: Arc<dyn Host>) -> Arc<Recorder> {
    let snapshot = Arc::new(Mutex::new(Snapshot {
        connection: "connected".into(),
        user: saved.user.clone(),
        host: echolot::host(&saved.base).to_owned(),
        device: saved.recorder.name.clone(),
        ..Snapshot::default()
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let recorder = Arc::new(Recorder { client: Some(client.clone()), snapshot: snapshot.clone(), stop: stop.clone() });
    std::thread::Builder::new()
        .name("echolot".into())
        .spawn(move || {
            init_thread();
            echolot_loop(&client, &profile, &out_dir, &host, &snapshot, &stop);
            host.detach();
        })
        .expect("echolot thread");
    recorder
}

/// An upload for the uploader thread.
struct Upload {
    item: Item,
    job: Job,
    file: PathBuf,
    /// Where it goes when kept: the same path relative to the output folder as to the waiting folder.
    keep_as: PathBuf,
    note: Option<String>,
}

fn echolot_loop(client: &Client, profile: &Profile, out_dir: &Path, shared_host: &Arc<dyn Host>, shared_snapshot: &Arc<Mutex<Snapshot>>, stop: &AtomicBool) {
    let (host, snapshot): (&dyn Host, &Mutex<Snapshot>) = (&**shared_host, shared_snapshot);
    let pending_dir = out_dir.join(PENDING_DIR);
    let not_taken = out_dir.join(NOT_TAKEN_DIR);
    // Who this device is (and whether the token still works).
    match client.me() {
        Ok(me) => publish(snapshot, host, |s| {
            s.user = me.user;
            s.device = me.recorder.name;
            s.session = me.session;
        }),
        Err(Error::Unauthorized) => return disconnected(snapshot, host),
        Err(e) => publish(snapshot, host, |s| {
            s.connection = "offline".into();
            s.error = Some(e.to_string());
        }),
    }
    let uploads = uploader(client.clone(), not_taken.clone(), shared_snapshot.clone(), shared_host.clone());
    let mut attached: Option<Attached> = None;
    let mut items: HashMap<u64, Item> = HashMap::new();
    // The song handed to the engine and not finished yet.
    let mut in_flight: Option<u64> = None;
    let mut heartbeat = Instant::now();
    let mut setup = (Instant::now() - SETUP_CHECK, None::<String>);
    let mut backoff = Duration::from_secs(5);
    let mut paused_for_sound = false;

    while !stop.load(Ordering::Relaxed) {
        // 1. Report what finished. An engine that went away (listening stopped by hand) ends the song too.
        let mut finished = Vec::new();
        if let Some(a) = &attached {
            loop {
                match a.done.try_recv() {
                    Ok(done) => finished.push(done),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        attached = None;
                        break;
                    }
                }
            }
        }
        // A song the engine never got to (it stopped by hand right after it was handed over).
        if attached.is_none()
            && let Some(job) = in_flight.and_then(|id| items.get(&id)).map(Item::job)
        {
            finished.push((job, Outcome::TookOver { detail: "listening was stopped".into() }));
        }
        for (job, outcome) in finished {
            let Some(item) = job.id.and_then(|id| items.remove(&id)) else { continue };
            if in_flight == Some(item.id) {
                in_flight = None;
            }
            match outcome {
                Outcome::Recorded { file, note } => {
                    let keep_as = file.strip_prefix(&pending_dir).map(Path::to_owned).unwrap_or_else(|_| file.file_name().unwrap_or_default().into());
                    let _ = uploads.send(Upload { item, job, file, keep_as, note });
                }
                other => {
                    let (result, detail) = result_of(&other);
                    if matches!(other, Outcome::NoSound) && !paused_for_sound {
                        paused_for_sound = true;
                        let _ = client.session("pause");
                    }
                    host.update(RecorderUpdate::Song { job: job.clone(), result: result.into(), detail: detail.clone() });
                    match client.result(&item, result, &detail) {
                        Ok(session) => publish(snapshot, host, |s| s.session = session.or(s.session.take())),
                        Err(Error::Unauthorized) => return disconnected(snapshot, host),
                        Err(e) => publish(snapshot, host, |s| s.error = Some(e.to_string())),
                    }
                }
            }
        }
        let status = attached.as_ref().and_then(|a| a.status.lock().ok().map(|s| s.clone()));
        let current = status.as_ref().and_then(|s| s.job.clone());

        // 2. Busy with a song: only a heartbeat now and then.
        if in_flight.is_some() {
            if heartbeat.elapsed() >= HEARTBEAT {
                heartbeat = Instant::now();
                let detail = match (&current, &status) {
                    // Echolot shows it next to the current song: the time reads best.
                    (Some(job), Some(s)) if s.recording => format!("{} / {}", clock(s.recorded_ms), clock(job.duration_ms)),
                    (Some(_), _) => "starting".into(),
                    _ => String::new(),
                };
                match client.poll(DeviceState::Recording, &detail, spotify_state(profile, status.as_ref()), in_flight, 0) {
                    Ok(poll) => publish(snapshot, host, |s| {
                        s.connection = "connected".into();
                        s.session = poll.session;
                        s.error = None;
                    }),
                    Err(Error::Unauthorized) => return disconnected(snapshot, host),
                    Err(e) => publish(snapshot, host, |s| s.error = Some(e.to_string())),
                }
            }
            publish(snapshot, host, |s| {
                s.state = Some(DeviceState::Recording);
                s.current = current;
            });
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }

        // 3. Ready: ask for work.
        if setup.0.elapsed() >= SETUP_CHECK {
            setup = (Instant::now(), setup_problem(profile));
        }
        let spotify = spotify_state(profile, status.as_ref());
        let (state, detail) = match (&setup.1, spotify) {
            (Some(problem), _) => (DeviceState::Setup, problem.clone()),
            (None, Spotify::Playing) => (DeviceState::Busy, String::new()),
            (None, _) => (DeviceState::Idle, String::new()),
        };
        publish(snapshot, host, |s| {
            s.state = Some(state);
            s.detail = detail.clone();
            s.current = None;
        });
        heartbeat = Instant::now();
        match client.poll(state, &detail, spotify, None, LONG_POLL_S) {
            Ok(poll) => {
                backoff = Duration::from_secs(5);
                let session_running = poll.session.as_ref().is_some_and(SessionInfo::running);
                if session_running {
                    paused_for_sound = false;
                }
                publish(snapshot, host, |s| {
                    s.connection = "connected".into();
                    s.session = poll.session.clone();
                    s.error = None;
                });
                match poll.item {
                    Some(item) if !stop.load(Ordering::Relaxed) => {
                        if attached.is_none() {
                            match attach(host, profile, &pending_dir) {
                                Ok(a) => attached = Some(a),
                                Err(e) => {
                                    // The song goes back to Echolot's queue.
                                    let _ = client.result(&item, "failed", &e);
                                    publish(snapshot, host, |s| s.error = Some(e));
                                    std::thread::sleep(Duration::from_secs(10));
                                    continue;
                                }
                            }
                        }
                        let job = item.job();
                        in_flight = Some(item.id);
                        items.insert(item.id, item);
                        publish(snapshot, host, |s| s.current = Some(job.clone()));
                        if let Some(a) = &attached
                            && a.jobs.send(job).is_err()
                        {
                            // The engine is gone: the next round reports the song as stalled.
                            attached = None;
                        }
                    }
                    _ => {
                        // Nothing to do and no running session: let the engine go (monitor level, volumes back).
                        if attached.is_some() && !session_running {
                            attached = None;
                            host.detach();
                        }
                    }
                }
            }
            Err(Error::Unauthorized) => return disconnected(snapshot, host),
            Err(e) => {
                publish(snapshot, host, |s| {
                    s.connection = "offline".into();
                    s.error = Some(e.to_string());
                });
                if attached.take().is_some() {
                    host.detach();
                }
                sleep_unless(stop, backoff);
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

fn disconnected(snapshot: &Mutex<Snapshot>, host: &dyn Host) {
    echolot::store::forget();
    publish(snapshot, host, |s| {
        s.connection = "disconnected".into();
        s.session = None;
        s.current = None;
        s.error = Some("Disconnected in Echolot".into());
    });
}

fn sleep_unless(stop: &AtomicBool, total: Duration) {
    let end = Instant::now() + total;
    while Instant::now() < end && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn clock(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, (ms / 1000) % 60)
}

/// The contract's result name and detail for an outcome without a file.
fn result_of(outcome: &Outcome) -> (&'static str, String) {
    match outcome {
        Outcome::Recorded { .. } => ("filed", String::new()),
        Outcome::LossyOnly { detail } => ("lossy_only", detail.clone()),
        Outcome::WrongSong { detail } => ("wrong_song", detail.clone()),
        Outcome::TookOver { detail } => ("took_over", detail.clone()),
        Outcome::Stalled { detail } => ("stalled", detail.clone()),
        Outcome::Failed { detail } => ("failed", detail.clone()),
        Outcome::NoSound => ("failed", "no sound on this PC (playing on another device?)".into()),
    }
}

/// Uploads recordings one by one; retries network and server errors for up to an hour.
fn uploader(client: Client, not_taken: PathBuf, snapshot: Arc<Mutex<Snapshot>>, host: Arc<dyn Host>) -> Sender<Upload> {
    let (tx, rx) = mpsc::channel::<Upload>();
    std::thread::Builder::new()
        .name("echolot-upload".into())
        .spawn(move || {
            for upload in rx {
                publish(&snapshot, &*host, |s| s.uploads += 1);
                let started = Instant::now();
                let mut pause = UPLOAD_RETRY_FIRST;
                let answer = loop {
                    match client.upload(&upload.item, &upload.file, upload.note.as_deref()) {
                        Err(e) if e.transient() && started.elapsed() < UPLOAD_GIVE_UP => {
                            publish(&snapshot, &*host, |s| s.error = Some(format!("upload of {} failed, retrying: {e}", upload.job.label())));
                            std::thread::sleep(pause);
                            pause = (pause * 2).min(UPLOAD_RETRY_MAX);
                        }
                        other => break other,
                    }
                };
                let (result, detail) = match answer {
                    Ok(filed) => {
                        // Rejected: kept for a look. Otherwise kept in the library, or deleted, as the settings say.
                        let settings = host.settings();
                        if filed.result == "rejected" {
                            move_to(&upload.file, &not_taken.join(&upload.keep_as));
                        } else if settings.keep_echolot_copies {
                            move_to(&upload.file, &settings.out_dir.join(&upload.keep_as));
                        } else {
                            let _ = std::fs::remove_file(&upload.file);
                        }
                        publish(&snapshot, &*host, |s| {
                            s.session = filed.session.clone().or(s.session.take());
                            s.error = None;
                        });
                        (filed.result, filed.detail)
                    }
                    Err(e) => {
                        move_to(&upload.file, &not_taken.join(&upload.keep_as));
                        if !e.transient() {
                            // Still worth telling Echolot, so the song goes back to its queue.
                            let _ = client.result(&upload.item, "failed", &e.to_string());
                        }
                        ("failed".into(), format!("upload: {e}"))
                    }
                };
                host.update(RecorderUpdate::Song { job: upload.job, result, detail });
                publish(&snapshot, &*host, |s| s.uploads = s.uploads.saturating_sub(1));
            }
        })
        .expect("upload thread");
    tx
}

/// Work through an exported list without a connection. The recordings stay in `<out_dir>/Echolot`, tagged with
/// their Spotify id and ISRC, for Echolot's upload dialog. A take-over or no sound ends the run.
pub fn run_list(export: Export, profile: Profile, out_dir: PathBuf, host: Arc<dyn Host>) -> Arc<Recorder> {
    let jobs = export.jobs();
    let snapshot = Arc::new(Mutex::new(Snapshot { connection: "file".into(), detail: export.title.clone(), total: jobs.len() as u32, ..Snapshot::default() }));
    let stop = Arc::new(AtomicBool::new(false));
    let recorder = Arc::new(Recorder { client: None, snapshot: snapshot.clone(), stop: stop.clone() });
    std::thread::Builder::new()
        .name("queue".into())
        .spawn(move || {
            init_thread();
            let (host, snapshot): (&dyn Host, &Mutex<Snapshot>) = (&*host, &snapshot);
            let attached = match attach(host, &profile, &out_dir.join(PENDING_DIR)) {
                Ok(a) => a,
                Err(e) => {
                    publish(snapshot, host, |s| {
                        s.connection = String::new();
                        s.error = Some(e);
                    });
                    return;
                }
            };
            'jobs: for job in jobs {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                publish(snapshot, host, |s| {
                    s.current = Some(job.clone());
                    s.state = Some(DeviceState::Recording);
                });
                let _ = attached.jobs.send(job);
                let outcome = loop {
                    match attached.done.recv_timeout(Duration::from_millis(500)) {
                        Ok((job, outcome)) => break (job, outcome),
                        Err(mpsc::RecvTimeoutError::Timeout) if stop.load(Ordering::Relaxed) => {
                            host.detach();
                            break 'jobs;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break 'jobs,
                    }
                };
                let (job, outcome) = outcome;
                let (result, detail) = match &outcome {
                    Outcome::Recorded { file, note } => ("recorded", note.clone().unwrap_or_else(|| file.display().to_string())),
                    other => result_of(other),
                };
                host.update(RecorderUpdate::Song { job, result: result.into(), detail: detail.clone() });
                publish(snapshot, host, |s| s.done += 1);
                if matches!(outcome, Outcome::TookOver { .. } | Outcome::NoSound) {
                    publish(snapshot, host, |s| s.error = Some(format!("Stopped: {detail}")));
                    break;
                }
            }
            host.detach();
            publish(snapshot, host, |s| {
                s.connection = String::new();
                s.current = None;
                s.state = None;
            });
        })
        .expect("queue thread");
    recorder
}

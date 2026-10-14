//! Works through Echolot's queue (long poll, one song at a time) or an exported list. Uploads run on their own thread
//! so the next song can start meanwhile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::echolot::{self, Client, DeviceState, Error, Export, Item, SessionInfo, Spotify};
use crate::engine::{Queue, QueueStatus};
use crate::profile::Profile;
use crate::runner::{Job, Outcome};
use crate::settings::Settings;
use crate::setup::{self, State};
use crate::{desktop, init_thread, media, sessions};

/// Recordings waiting for Echolot's answer.
pub const PENDING_DIR: &str = "Echolot";
pub const NOT_TAKEN_DIR: &str = "Not taken";

const LONG_POLL_S: u32 = 25;
/// Spotify Connect may play on another device while the app here shows "playing".
const BUSY: &str = "Spotify is playing (here or on another device)";
const NO_SOUND: &str = "no sound from Spotify on this PC – is it playing on another device (Spotify Connect)?";
const HEARTBEAT: Duration = Duration::from_secs(10);
const SETUP_CHECK: Duration = Duration::from_secs(60);
const PLAYER_START: Duration = Duration::from_secs(30);
const UPLOAD_RETRY_FIRST: Duration = Duration::from_secs(10);
const UPLOAD_RETRY_MAX: Duration = Duration::from_secs(300);
const UPLOAD_GIVE_UP: Duration = Duration::from_secs(3_600);

/// The app or the CLI, which own the engine.
pub trait Host: Send + Sync + 'static {
    /// Attaches to the running engine, or starts one.
    fn attach(&self, queue: Queue) -> Result<(), String>;
    fn detach(&self);
    fn settings(&self) -> Settings;
    fn update(&self, update: RecorderUpdate);
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    /// `connected`, `offline`, `disconnected`, or `file` (an exported list).
    pub connection: String,
    pub user: String,
    pub host: String,
    pub device: String,
    pub state: Option<DeviceState>,
    pub detail: String,
    pub session: Option<SessionInfo>,
    pub current: Option<Job>,
    pub uploads: usize,
    pub error: Option<String>,
    /// Exported list progress.
    pub done: u32,
    pub total: u32,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecorderUpdate {
    Snapshot { snapshot: Snapshot },
    /// `moved`: the uploaded file and where it went (`None`: deleted).
    Song {
        job: Job,
        result: String,
        detail: String,
        #[serde(skip)]
        moved: Option<(PathBuf, Option<PathBuf>)>,
    },
}

pub struct Recorder {
    pub client: Option<Client>,
    snapshot: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
}

impl Recorder {
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn session(&self, action: &str, host: &dyn Host) -> Result<(), String> {
        let client = self.client.as_ref().ok_or("not connected")?;
        let session = client.session(action).map_err(|e| e.to_string())?;
        let snapshot = {
            let mut s = self.snapshot.lock().map_err(|e| e.to_string())?;
            s.session = session;
            s.clone()
        };
        host.update(RecorderUpdate::Snapshot { snapshot });
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
    host.update(RecorderUpdate::Snapshot { snapshot });
}

pub fn ensure_player(profile: &Profile) -> Result<(), String> {
    let running = || sessions::find_app(profile.exe, &sessions::processes()).is_some();
    if running() {
        return Ok(());
    }
    desktop::open_link_in_background(&format!("{}:", profile.id), profile.exe)?;
    let deadline = Instant::now() + PLAYER_START;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        if running() {
            // Links opened while it still starts up get lost.
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

fn setup_problem(profile: &Profile) -> Option<String> {
    setup::health(profile).into_iter().find(|c| c.state == State::Bad).map(|c| format!("{}: {}", c.title, c.detail))
}

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

/// As "name (2).flac" etc. when `target` exists.
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

/// `<file>.json` next to a recording until Echolot answered, so a crash doesn't lose the upload.
#[derive(Serialize, Deserialize)]
struct Sidecar {
    item: Item,
    note: Option<String>,
}

fn sidecar(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".json");
    PathBuf::from(name)
}

fn leftover_uploads(pending_dir: &Path) -> Vec<Upload> {
    let mut found = Vec::new();
    let mut dirs = vec![pending_dir.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let Some(file) = path.to_str().and_then(|p| p.strip_suffix(".json")).map(PathBuf::from) else { continue };
            let sidecar: Option<Sidecar> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok());
            match sidecar {
                Some(Sidecar { item, note }) if file.exists() => {
                    let keep_as = file.strip_prefix(pending_dir).map(Path::to_owned).unwrap_or_default();
                    found.push(Upload { job: item.job(), item, file, keep_as, note });
                }
                _ => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
    found
}

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

struct Upload {
    item: Item,
    job: Job,
    file: PathBuf,
    /// Relative to the output folder when kept.
    keep_as: PathBuf,
    note: Option<String>,
}

fn echolot_loop(client: &Client, profile: &Profile, out_dir: &Path, shared_host: &Arc<dyn Host>, shared_snapshot: &Arc<Mutex<Snapshot>>, stop: &AtomicBool) {
    let (host, snapshot): (&dyn Host, &Mutex<Snapshot>) = (&**shared_host, shared_snapshot);
    let pending_dir = out_dir.join(PENDING_DIR);
    let not_taken = out_dir.join(NOT_TAKEN_DIR);
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
    let mut in_flight: Option<u64> = None;
    let mut heartbeat = Instant::now();
    let mut setup = (Instant::now() - SETUP_CHECK, None::<String>);
    let mut backoff = Duration::from_secs(5);
    let mut paused_for_sound = false;
    let mut logged = (DeviceState::Idle, "-".to_owned());
    // The first poll tells Echolot to release what a crashed run had leased.
    let mut fresh = true;
    for upload in leftover_uploads(&pending_dir) {
        let _ = uploads.send(upload);
    }

    while !stop.load(Ordering::Relaxed) {
        // 1. Report what finished.
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
        // Listening was stopped by hand.
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
                    // Survives a close or crash before the upload (see `leftover_uploads`).
                    if let Ok(json) = serde_json::to_string(&Sidecar { item: item.clone(), note: note.clone() }) {
                        let _ = std::fs::write(sidecar(&file), json);
                    }
                    let _ = uploads.send(Upload { item, job, file, keep_as, note });
                }
                other => {
                    let (result, detail) = result_of(&other);
                    crate::log!("echolot result {} {}: {result} \"{detail}\"", item.id, job.label());
                    if matches!(other, Outcome::NoSound) && !paused_for_sound {
                        paused_for_sound = true;
                        let _ = client.session("pause");
                    }
                    host.update(RecorderUpdate::Song { job: job.clone(), result: result.into(), detail: detail.clone(), moved: None });
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
                match client.poll(DeviceState::Recording, &detail, spotify_state(profile, status.as_ref()), in_flight, 0, false) {
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
            (None, Spotify::Playing) => (DeviceState::Busy, BUSY.into()),
            (None, _) => (DeviceState::Idle, String::new()),
        };
        if logged != (state, detail.clone()) {
            crate::log!("echolot state {state:?} {detail}");
            logged = (state, detail.clone());
        }
        publish(snapshot, host, |s| {
            s.state = Some(state);
            s.detail = detail.clone();
            s.current = None;
        });
        heartbeat = Instant::now();
        match client.poll(state, &detail, spotify, None, LONG_POLL_S, fresh) {
            Ok(poll) => {
                fresh = false;
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
                        crate::log!("echolot item {} {}", item.id, job.label());
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
                crate::log!("echolot offline: {e}");
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

fn result_of(outcome: &Outcome) -> (&'static str, String) {
    match outcome {
        Outcome::Recorded { .. } => ("filed", String::new()),
        Outcome::LossyOnly { detail } => ("lossy_only", detail.clone()),
        Outcome::WrongSong { detail } => ("wrong_song", detail.clone()),
        Outcome::TookOver { detail } => ("took_over", detail.clone()),
        Outcome::Stalled { detail } => ("stalled", detail.clone()),
        Outcome::Failed { detail } => ("failed", detail.clone()),
        // Not the song's fault: it goes back without costing a try.
        Outcome::NoSound => ("took_over", NO_SOUND.into()),
    }
}

/// Retries network and server errors for up to an hour.
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
                let (result, detail, to) = match answer {
                    Ok(filed) => {
                        let settings = host.settings();
                        let to = if filed.result == "rejected" {
                            move_to(&upload.file, &not_taken.join(&upload.keep_as))
                        } else if settings.keep_echolot_copies {
                            move_to(&upload.file, &settings.out_dir.join(&upload.keep_as))
                        } else {
                            let _ = std::fs::remove_file(&upload.file);
                            None
                        };
                        publish(&snapshot, &*host, |s| {
                            s.session = filed.session.clone().or(s.session.take());
                            s.error = None;
                        });
                        (filed.result, filed.detail, to)
                    }
                    Err(e) => {
                        let to = move_to(&upload.file, &not_taken.join(&upload.keep_as));
                        if !e.transient() {
                            // Still worth telling Echolot, so the song goes back to its queue.
                            let _ = client.result(&upload.item, "failed", &e.to_string());
                        }
                        ("failed".into(), format!("upload: {e}"), to)
                    }
                };
                crate::log!("upload {}: {result} \"{detail}\"", upload.job.label());
                let _ = std::fs::remove_file(sidecar(&upload.file));
                host.update(RecorderUpdate::Song { job: upload.job, result, detail, moved: Some((upload.file, to)) });
                publish(&snapshot, &*host, |s| s.uploads = s.uploads.saturating_sub(1));
            }
        })
        .expect("upload thread");
    tx
}

/// Without a connection: the recordings stay in the waiting folder for Echolot's upload dialog.
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
                host.update(RecorderUpdate::Song { job, result: result.into(), detail: detail.clone(), moved: None });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploads_left_by_a_crash_are_found_again() {
        let dir = std::env::temp_dir().join(format!("mitschnitt-leftover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Artist")).unwrap();
        let item: Item = serde_json::from_str(r#"{"id": 7, "uri": "spotify:track:x", "artist": "Artist", "title": "Song", "duration_ms": 1000, "upload": "/up/7", "result": "/res/7"}"#).unwrap();
        let file = dir.join("Artist").join("Song.flac");
        std::fs::write(&file, b"flac").unwrap();
        std::fs::write(sidecar(&file), serde_json::to_string(&Sidecar { item: item.clone(), note: Some("faded".into()) }).unwrap()).unwrap();
        std::fs::write(dir.join("Gone.flac.json"), "{}").unwrap();
        std::fs::write(dir.join("Other.flac"), b"flac").unwrap();

        let found = leftover_uploads(&dir);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].item.id, found[0].note.as_deref(), found[0].keep_as.clone()), (7, Some("faded"), PathBuf::from("Artist").join("Song.flac")));
        assert!(!dir.join("Gone.flac.json").exists() && dir.join("Other.flac").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

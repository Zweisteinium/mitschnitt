//! The recorder shares the app's engine: it attaches to a running one or starts one for its songs.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use mitschnitt_core::echolot::{self, Client, Export, Link, Pairing, store};
use mitschnitt_core::engine::{Config, Engine, Queue};
use mitschnitt_core::history;
use mitschnitt_core::profile::SPOTIFY;
use mitschnitt_core::recorder::{self, Host, Recorder, RecorderUpdate, Snapshot};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{App, lock};

#[derive(Default)]
pub struct Echolot {
    recorder: Mutex<Option<Arc<Recorder>>>,
    list: Mutex<Option<Arc<Recorder>>>,
    pairing: Mutex<Option<Pairing>>,
}

impl Echolot {
    fn list_running(&self) -> bool {
        lock(&self.list).is_ok_and(|l| l.as_ref().is_some_and(|r| !r.snapshot().connection.is_empty()))
    }
}

struct AppHost {
    handle: AppHandle,
    list: bool,
}

impl Host for AppHost {
    fn attach(&self, queue: Queue) -> Result<(), String> {
        // One engine, one queue: Echolot's songs are refused while a list runs.
        if !self.list && self.handle.state::<Echolot>().list_running() {
            return Err("a list is being recorded on this PC".into());
        }
        let state = self.handle.state::<App>();
        let mut slot = lock(&state.engine)?;
        // An engine that stopped by itself (Spotify closed) gives the queue back: start a new one.
        let queue = match slot.as_ref() {
            Some(engine) => match engine.attach(queue) {
                Ok(()) => return Ok(()),
                Err(queue) => queue,
            },
            None => queue,
        };
        let settings = lock(&state.settings)?.clone();
        let handle = self.handle.clone();
        let (engine, info) = Engine::start(Config { profile: SPOTIFY, settings, trace: None, queue: Some(queue) }, move |update| crate::notify(&handle, update))?;
        *slot = Some(engine);
        state.for_queue.store(true, Ordering::Relaxed);
        let _ = self.handle.emit("listening", &info);
        Ok(())
    }

    fn detach(&self) {
        let state = self.handle.state::<App>();
        let Ok(mut slot) = lock(&state.engine) else { return };
        if state.for_queue.swap(false, Ordering::Relaxed) {
            slot.take(); // stops, restores volumes; the UI gets `stopped`
        } else if let Some(engine) = slot.as_ref() {
            engine.detach();
        }
    }

    fn settings(&self) -> mitschnitt_core::settings::Settings {
        self.handle.state::<App>().settings.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn update(&self, update: RecorderUpdate) {
        if let RecorderUpdate::Song { result, moved: Some((from, to)), .. } = &update {
            let to = to.as_ref().map(|p| p.display().to_string());
            history::moved(&history::default_path(), &from.display().to_string(), to.as_deref(), result);
        }
        let _ = self.handle.emit(if self.list { "echolot-list" } else { "echolot" }, &update);
    }
}

fn host(handle: &AppHandle) -> Arc<dyn Host> {
    Arc::new(AppHost { handle: handle.clone(), list: false })
}

fn out_dir(handle: &AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(lock(&handle.state::<App>().settings)?.out_dir.clone())
}

pub fn resume(handle: &AppHandle) -> Result<(), String> {
    let Some((saved, token)) = store::load() else { return Ok(()) };
    let state = handle.state::<Echolot>();
    let mut slot = lock(&state.recorder)?;
    if let Some(old) = slot.take() {
        old.stop();
    }
    let client = Client::new(&saved.base, &token);
    *slot = Some(recorder::connect(client, saved, SPOTIFY, out_dir(handle)?, host(handle)));
    Ok(())
}

pub fn open_link(handle: &AppHandle, link: &str) {
    if let Some(window) = handle.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    match echolot::parse_link(link) {
        // Started from Echolot's account page: the card asks to confirm the address.
        Some(Link::Connect { url }) => {
            let _ = handle.emit("echolot-connect", &url);
        }
        Some(Link::Connected { error: Some(error), .. }) => {
            let _ = handle.emit("echolot-error", format!("Not connected: {error}"));
        }
        Some(Link::Connected { code: Some(code), state, .. }) => {
            let handle = handle.clone();
            std::thread::spawn(move || {
                if let Err(e) = claim(&handle, &code, Some(&state)) {
                    let _ = handle.emit("echolot-error", e);
                }
            });
        }
        _ => {}
    }
}

/// `state` is not checked for a pasted code.
fn claim(handle: &AppHandle, code: &str, state: Option<&str>) -> Result<(), String> {
    let pairing = lock(&handle.state::<Echolot>().pairing)?.clone().ok_or("No sign-in is waiting – press Connect again.")?;
    if state.is_some_and(|s| s != pairing.state) {
        return Err("That link belongs to another sign-in – press Connect again.".into());
    }
    let connected = pairing.claim(code, &echolot::pc_name()).map_err(|e| e.to_string())?;
    let saved = store::Saved { base: pairing.base.clone(), user: connected.user, recorder: connected.recorder };
    store::save(&saved, &connected.token)?;
    *lock(&handle.state::<Echolot>().pairing)? = None;
    let state = handle.state::<App>();
    {
        let mut settings = lock(&state.settings)?;
        settings.echolot_url = pairing.base.clone();
        let _ = settings.save(&mitschnitt_core::settings::Settings::default_path());
    }
    resume(handle)
}

#[derive(serde::Serialize)]
pub struct CardState {
    snapshot: Snapshot,
    list: Option<Snapshot>,
    url: String,
    pairing: bool,
}

#[tauri::command]
pub fn echolot_state(state: State<'_, Echolot>, app: State<'_, App>) -> Result<CardState, String> {
    let snapshot = lock(&state.recorder)?.as_ref().map(|r| r.snapshot()).unwrap_or_else(|| Snapshot { connection: if store::load().is_some() { "connected".into() } else { String::new() }, ..Snapshot::default() });
    let url = lock(&app.settings)?.echolot_url.clone();
    let list = lock(&state.list)?.as_ref().map(|r| r.snapshot());
    Ok(CardState { snapshot, list, url, pairing: lock(&state.pairing)?.is_some() })
}

#[tauri::command]
pub fn echolot_connect(url: String, state: State<'_, Echolot>) -> Result<(), String> {
    let pairing = Pairing::new(&url);
    mitschnitt_core::desktop::open(&pairing.url(&echolot::pc_name()))?;
    *lock(&state.pairing)? = Some(pairing);
    Ok(())
}

/// The code from Echolot's page, when the link didn't open Mitschnitt (a pasted link works too).
#[tauri::command]
pub async fn echolot_code(code: String, handle: AppHandle) -> Result<(), String> {
    let code = match echolot::parse_link(&code) {
        Some(Link::Connected { code: Some(code), .. }) => code,
        _ => code.trim().to_owned(),
    };
    claim(&handle, &code, None)
}

#[tauri::command]
pub fn echolot_cancel(state: State<'_, Echolot>) -> Result<(), String> {
    *lock(&state.pairing)? = None;
    Ok(())
}

#[tauri::command]
pub async fn echolot_disconnect(handle: AppHandle) -> Result<(), String> {
    let state = handle.state::<Echolot>();
    let recorder = lock(&state.recorder)?.take();
    if let Some(r) = &recorder {
        r.stop();
        if let Some(client) = &r.client {
            let _ = client.disconnect();
        }
    } else if let Some((saved, token)) = store::load() {
        let _ = Client::new(&saved.base, &token).disconnect();
    }
    store::forget();
    let _ = handle.emit("echolot", RecorderUpdate::Snapshot { snapshot: Snapshot::default() });
    Ok(())
}

#[tauri::command]
pub fn echolot_skip(state: State<'_, Echolot>) -> Result<(), String> {
    lock(&state.recorder)?.as_ref().ok_or("not connected")?.skip();
    Ok(())
}

#[tauri::command]
pub async fn echolot_session(action: String, handle: AppHandle) -> Result<(), String> {
    let recorder = lock(&handle.state::<Echolot>().recorder)?.clone().ok_or("not connected")?;
    recorder.session(&action, &*host(&handle))
}


#[tauri::command]
pub fn echolot_list(json: String, handle: AppHandle) -> Result<(), String> {
    let export = Export::parse(&json)?;
    if export.jobs().is_empty() {
        return Err("The list has no Spotify songs.".into());
    }
    let state = handle.state::<Echolot>();
    if state.list_running() {
        return Err("A list is already being recorded.".into());
    }
    let echolot = lock(&state.recorder)?.as_ref().map(|r| r.snapshot());
    if echolot.as_ref().is_some_and(|s| s.current.is_some() || s.session.as_ref().is_some_and(|s| s.state == "running")) {
        return Err("Pause the Echolot session first.".into());
    }
    let host: Arc<dyn Host> = Arc::new(AppHost { handle: handle.clone(), list: true });
    *lock(&state.list)? = Some(recorder::run_list(export, SPOTIFY, out_dir(&handle)?, host));
    Ok(())
}

#[tauri::command]
pub fn echolot_list_control(action: String, handle: AppHandle) -> Result<(), String> {
    let list = lock(&handle.state::<Echolot>().list)?.clone().ok_or("no list")?;
    match action.as_str() {
        "skip" => list.skip(),
        "pause" => list.pause(true),
        "resume" => list.pause(false),
        _ => list.stop(),
    }
    let _ = handle.emit("echolot-list", RecorderUpdate::Snapshot { snapshot: list.snapshot() });
    Ok(())
}

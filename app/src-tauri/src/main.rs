#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod echolot;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use mitschnitt_core::engine::{self, Config, Engine, SourceInfo, Update};
use mitschnitt_core::history::{self, Record};
use mitschnitt_core::media::{self, Source};
use mitschnitt_core::profile::{self, Profile};
use mitschnitt_core::settings::Settings;
use mitschnitt_core::setup::{self, Check};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};
use tauri_plugin_deep_link::DeepLinkExt;

pub(crate) struct App {
    pub(crate) engine: Mutex<Option<Engine>>,
    pub(crate) settings: Mutex<Settings>,
    /// Started for Echolot's queue, so stopped again when the queue lets it go.
    pub(crate) for_queue: AtomicBool,
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, String> {
    m.lock().map_err(|e| e.to_string())
}

#[tauri::command]
async fn sources() -> Result<Vec<Source>, String> {
    mitschnitt_core::init_thread();
    media::sources().map_err(|e| e.to_string())
}

fn supported(app: &str) -> Result<&'static Profile, String> {
    profile::get(app).ok_or_else(|| format!("\"{app}\" is not supported"))
}

#[tauri::command]
async fn start(app: String, handle: AppHandle, state: State<'_, App>) -> Result<SourceInfo, String> {
    let profile = supported(&app)?.clone();
    let mut slot = lock(&state.engine)?;
    if slot.is_some() {
        return Err("Already listening.".into());
    }
    let settings = lock(&state.settings)?.clone();
    let (engine, info) = Engine::start(Config { profile, settings, trace: None, queue: None }, move |update| notify(&handle, update))?;
    *slot = Some(engine);
    Ok(info)
}

pub(crate) fn notify(handle: &AppHandle, update: Update) {
    if let Update::Saved { track, file: Some(file), size, verdict, master, exact, faded, lossy, issues, echolot, frames, peak_dbfs } = &update {
        let record = Record { at: history::now_ms(), track: track.clone(), file: file.clone(), size: *size, verdict: *verdict, master: *master, exact: *exact, faded: *faded, lossy: *lossy, echolot: *echolot, result: None, issues: issues.clone(), frames: Some(*frames), peak_dbfs: *peak_dbfs, exists: true };
        history::add(&history::default_path(), &record);
    }
    let _ = handle.emit("update", &update);
}

#[tauri::command]
async fn history() -> Vec<Record> {
    history::load(&history::default_path())
}

#[tauri::command]
async fn stop(state: State<'_, App>) -> Result<(), String> {
    state.for_queue.store(false, Ordering::Relaxed);
    let engine = lock(&state.engine)?.take();
    drop(engine); // stops capture, discards an unfinished track, restores volumes
    Ok(())
}

/// What is being listened to: Echolot may have started it before the window was there to hear of it.
#[tauri::command]
fn listening(state: State<'_, App>) -> Result<Option<SourceInfo>, String> {
    Ok(lock(&state.engine)?.as_ref().map(|e| e.info().clone()))
}

#[tauri::command]
fn stop_after_track(state: State<'_, App>) -> Result<(), String> {
    if let Some(engine) = lock(&state.engine)?.as_ref() {
        engine.stop_after_track();
    }
    Ok(())
}

#[tauri::command]
fn unmute_monitor(state: State<'_, App>) -> Result<(), String> {
    if let Some(engine) = lock(&state.engine)?.as_ref() {
        engine.unmute_monitor();
    }
    Ok(())
}

/// Forget an engine that stopped by itself (app closed, stop after track).
#[tauri::command]
fn release(state: State<'_, App>) -> Result<(), String> {
    state.for_queue.store(false, Ordering::Relaxed);
    lock(&state.engine)?.take();
    Ok(())
}

#[tauri::command]
fn get_settings(state: State<'_, App>) -> Result<Settings, String> {
    Ok(lock(&state.settings)?.clone())
}

#[tauri::command]
fn set_monitor_volume(volume: f32, state: State<'_, App>) -> Result<(), String> {
    if let Some(engine) = lock(&state.engine)?.as_ref() {
        engine.set_monitor_volume(volume);
    }
    Ok(())
}

#[tauri::command]
fn save_settings(settings: Settings, state: State<'_, App>) -> Result<(), String> {
    settings.save(&Settings::default_path()).map_err(|e| e.to_string())?;
    *lock(&state.settings)? = settings;
    Ok(())
}

#[tauri::command]
async fn health(app: String) -> Result<Vec<Check>, String> {
    Ok(setup::health(supported(&app)?))
}

#[tauri::command]
async fn fix(action: String, app: String) -> Result<String, String> {
    setup::fix(&action, supported(&app)?)
}

#[tauri::command]
fn open(target: String) -> Result<(), String> {
    mitschnitt_core::desktop::open(&target)
}

#[tauri::command]
fn play(file: String) -> Result<(), String> {
    mitschnitt_core::desktop::open_link(&file)
}

#[tauri::command]
fn open_log() -> Result<(), String> {
    mitschnitt_core::desktop::open(&mitschnitt_core::log::path().to_string_lossy())
}

#[derive(serde::Serialize)]
struct Platform {
    os: &'static str,
    monitor: bool,
    media_keys: bool,
    gain: f32,
}

#[tauri::command]
fn platform() -> Platform {
    Platform { os: std::env::consts::OS, monitor: mitschnitt_core::monitor::SUPPORTED, media_keys: mitschnitt_core::guard::SUPPORTED, gain: engine::GAIN }
}

fn main() {
    mitschnitt_core::log::init("app");
    // Repair volumes a previous run left behind (crash or killed process), off the UI thread.
    std::thread::spawn(engine::recover);
    let settings = Settings::load(&Settings::default_path());
    tauri::Builder::default()
        // Must be first: a second start (a `mitschnitt://` link) hands its link to this instance.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .manage(App { engine: Mutex::new(None), settings: Mutex::new(settings), for_queue: AtomicBool::new(false) })
        .manage(echolot::Echolot::default())
        .setup(|app| {
            // Portable exe, AppImage and dev builds register the scheme themselves (the installer does it too).
            let _ = app.deep_link().register_all();
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    echolot::open_link(&handle, url.as_str());
                }
            });
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                for url in urls {
                    echolot::open_link(app.handle(), url.as_str());
                }
            }
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                if let Err(e) = echolot::resume(&handle) {
                    let _ = handle.emit("echolot-error", e);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            sources,
            start,
            stop,
            stop_after_track,
            listening,
            release,
            unmute_monitor,
            set_monitor_volume,
            get_settings,
            save_settings,
            health,
            fix,
            open,
            open_log,
            play,
            platform,
            history,
            echolot::echolot_state,
            echolot::echolot_connect,
            echolot::echolot_code,
            echolot::echolot_cancel,
            echolot::echolot_disconnect,
            echolot::echolot_session,
            echolot::echolot_skip
        ])
        .build(tauri::generate_context!())
        .expect("error while building Mitschnitt")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                // Drop the engine explicitly so volumes are restored before the process ends.
                if let Ok(mut slot) = app.state::<App>().engine.lock() {
                    slot.take();
                }
            }
        });
}

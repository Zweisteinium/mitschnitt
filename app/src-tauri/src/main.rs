#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod echolot;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use mitschnitt_core::engine::{self, Config, Engine, SourceInfo};
use mitschnitt_core::media::{self, Source};
use mitschnitt_core::profile::{self, Profile};
use mitschnitt_core::settings::Settings;
use mitschnitt_core::setup::{self, Check};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};
use tauri_plugin_deep_link::DeepLinkExt;

pub(crate) struct App {
    pub(crate) engine: Mutex<Option<Engine>>,
    pub(crate) settings: Mutex<Settings>,
    /// The running engine was started for Echolot's queue (and is stopped again when the queue lets it go).
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
    let (engine, info) = Engine::start(Config { profile, settings, trace: None, queue: None }, move |update| {
        let _ = handle.emit("update", &update);
    })?;
    *slot = Some(engine);
    Ok(info)
}

#[tauri::command]
async fn stop(state: State<'_, App>) -> Result<(), String> {
    state.for_queue.store(false, Ordering::Relaxed);
    let engine = lock(&state.engine)?.take();
    drop(engine); // stops capture, discards an unfinished track, restores volumes
    Ok(())
}

/// Finish the current track, then stop (the engine reports `Stopped`).
#[tauri::command]
fn stop_after_track(state: State<'_, App>) -> Result<(), String> {
    if let Some(engine) = lock(&state.engine)?.as_ref() {
        engine.stop_after_track();
    }
    Ok(())
}

/// Unmute a monitor that muted itself because the output device changed.
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

/// Monitor volume = the app's Windows mixer entry (applies immediately, remembered by Windows).
#[tauri::command]
fn set_monitor_volume(volume: f32, state: State<'_, App>) -> Result<(), String> {
    if let Some(engine) = lock(&state.engine)?.as_ref() {
        engine.set_monitor_volume(volume);
    }
    Ok(())
}

/// Save settings (applied to the next start).
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

/// Show a file in the file manager, or open a folder or web link.
#[tauri::command]
fn open(target: String) -> Result<(), String> {
    mitschnitt_core::desktop::open(&target)
}

/// What this system offers, so the UI only shows what works here.
#[derive(serde::Serialize)]
struct Platform {
    os: &'static str,
    monitor: bool,
    media_keys: bool,
    /// App volume while listening (0..1).
    gain: f32,
}

#[tauri::command]
fn platform() -> Platform {
    Platform { os: std::env::consts::OS, monitor: mitschnitt_core::monitor::SUPPORTED, media_keys: mitschnitt_core::guard::SUPPORTED, gain: engine::GAIN }
}

fn main() {
    // Repair volumes a previous run left behind (crash or killed process), off the UI thread.
    std::thread::spawn(engine::recover);
    let settings = Settings::load(&Settings::default_path());
    tauri::Builder::default()
        // First: a second start (a `mitschnitt://` link opened while Mitschnitt runs) hands its link to this one.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(App { engine: Mutex::new(None), settings: Mutex::new(settings), for_queue: AtomicBool::new(false) })
        .manage(echolot::Echolot::default())
        .setup(|app| {
            // Portable exe and dev builds register the scheme themselves (the installer does it too).
            #[cfg(windows)]
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
            // A connected device asks Echolot for work whenever Mitschnitt is open.
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
            release,
            unmute_monitor,
            set_monitor_volume,
            get_settings,
            save_settings,
            health,
            fix,
            open,
            platform,
            echolot::echolot_state,
            echolot::echolot_connect,
            echolot::echolot_code,
            echolot::echolot_cancel,
            echolot::echolot_disconnect,
            echolot::echolot_session,
            echolot::import_queue,
            echolot::stop_queue
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

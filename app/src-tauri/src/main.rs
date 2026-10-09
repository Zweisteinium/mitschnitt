#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;

use mitschnitt_core::devices::com_init;
use mitschnitt_core::engine::{self, Config, Engine, SourceInfo};
use mitschnitt_core::media::{self, Source};
use mitschnitt_core::profile::{self, Profile};
use mitschnitt_core::settings::Settings;
use mitschnitt_core::setup::{self, Check};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};

struct App {
    engine: Mutex<Option<Engine>>,
    settings: Mutex<Settings>,
}

fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, String> {
    m.lock().map_err(|e| e.to_string())
}

#[tauri::command]
async fn sources() -> Result<Vec<Source>, String> {
    com_init();
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
    let (engine, info) = Engine::start(Config { profile, settings }, move |update| {
        let _ = handle.emit("update", &update);
    })?;
    *slot = Some(engine);
    Ok(info)
}

#[tauri::command]
async fn stop(state: State<'_, App>) -> Result<(), String> {
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

/// Forget an engine that stopped by itself (app closed, stop after track).
#[tauri::command]
fn release(state: State<'_, App>) -> Result<(), String> {
    lock(&state.engine)?.take();
    Ok(())
}

#[tauri::command]
fn get_settings(state: State<'_, App>) -> Result<Settings, String> {
    Ok(lock(&state.settings)?.clone())
}

/// Save settings (applied to the next start; the monitor volume applies immediately).
#[tauri::command]
fn save_settings(settings: Settings, state: State<'_, App>) -> Result<(), String> {
    if let Some(engine) = lock(&state.engine)?.as_ref() {
        engine.set_monitor_volume(settings.monitor_volume);
    }
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

/// Show a file or folder in Explorer, or open a web link.
#[tauri::command]
fn open(target: String) -> Result<(), String> {
    let arg = if std::path::Path::new(&target).is_file() { format!("/select,{target}") } else { target };
    std::process::Command::new("explorer").arg(arg).spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn main() {
    // Repair volumes a previous run left behind (crash or killed process), off the UI thread.
    std::thread::spawn(engine::recover);
    let settings = Settings::load(&Settings::default_path());
    tauri::Builder::default()
        .manage(App { engine: Mutex::new(None), settings: Mutex::new(settings) })
        .invoke_handler(tauri::generate_handler![sources, start, stop, stop_after_track, release, get_settings, save_settings, health, fix, open])
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

//! Read-only checks: no cable to install, PipeWire hands over the app's own stream.

use super::GAIN;
use super::devices::describe;
use super::pw::Conn;
use super::sessions::App;
use crate::profile::Profile;
use crate::setup::{Check, State, check, spotify_checks};
use crate::{media, sessions};

pub fn health(profile: &Profile) -> Vec<Check> {
    let conn = match Conn::new() {
        Ok(conn) => conn,
        Err(e) => return vec![check("pipewire", "PipeWire", State::Bad, format!("{e}. Mitschnitt needs PipeWire as the sound server."), None)],
    };
    let mut out = vec![check("pipewire", "PipeWire", State::Ok, "Running", None)];
    let graph = conn.graph();
    let procs = sessions::processes();
    let app = sessions::find_app(profile.exe, &procs).map(|root| App::new(sessions::tree(root.pid, &procs)));
    let streams: Vec<u32> = app.as_ref().map(|a| a.streams(&graph).map(|s| s.id).collect()).unwrap_or_default();
    let name = profile.name;

    match (&app, app.as_ref().and_then(|a| a.sink(&graph))) {
        (None, _) => out.push(check("route", "App output", State::Check, format!("{name} is not running"), None)),
        (Some(_), _) if streams.is_empty() => out.push(check("route", "App output", State::Check, format!("{name} has no audio stream yet - it opens one when playback starts"), None)),
        (Some(_), None) => out.push(check("route", "App output", State::Warn, format!("{name}'s stream is not connected to an output"), None)),
        (Some(_), Some(sink)) => {
            let device = describe(&conn, &graph, sink);
            out.push(check("route", "App output", State::Ok, format!("{name} plays on \"{}\"", device.name), None));
            out.push(rate_check(profile, streams.iter().find_map(|s| conn.format_rate(*s))));
        }
    }

    let volumes: Vec<f32> = streams.iter().filter_map(|s| conn.volume(*s).ok()).flat_map(|v| v.effective()).collect();
    let percent = |v: f32| format!("{:.0} %", v * 100.0);
    if let Some(off) = volumes.iter().find(|v| **v != GAIN) {
        let detail = format!("{} - Mitschnitt holds it at exactly {} while listening; use the system volume for loudness.", percent(*off), percent(GAIN));
        out.push(check("volume", "App volume", State::Warn, detail, None));
    } else if !volumes.is_empty() {
        out.push(check("volume", "App volume", State::Ok, percent(GAIN), None));
    }

    out.push(match media::find(profile) {
        Ok(Some(_)) => check("media", "Media session", State::Ok, format!("{name} reports track and position"), None),
        Ok(None) => check("media", "Media session", State::Check, format!("No media session of {name} - start it first"), None),
        Err(e) => check("media", "Media session", State::Bad, e, None),
    });
    if profile.id == "spotify" {
        out.extend(spotify_checks().into_iter().filter(|c| c.id != "exclusive"));
    }
    out
}

/// While listening, PipeWire runs at the app's rate, so the app's own rate is what matters.
fn rate_check(profile: &Profile, app_rate: Option<u32>) -> Check {
    let khz = |r: u32| r as f64 / 1000.0;
    match app_rate {
        Some(rate) if rate != profile.rate => {
            check("rate", "Sample rate", State::Bad, format!("{} delivers {} kHz instead of {} kHz", profile.name, khz(rate), khz(profile.rate)), None)
        }
        _ => check("rate", "Sample rate", State::Ok, format!("{} kHz - PipeWire runs at it while listening", khz(profile.rate)), None),
    }
}

pub fn fix(action: &str, _profile: &Profile) -> Result<String, String> {
    Err(format!("\"{action}\" is not available on Linux"))
}

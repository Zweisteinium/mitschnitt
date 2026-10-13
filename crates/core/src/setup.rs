//! Health checks of everything that affects a bit-exact recording, with one-click fixes where possible.

use std::path::PathBuf;

use serde::Serialize;

pub use crate::platform::os::setup::{fix, health};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ok,
    Warn,
    Bad,
    /// Can't be detected; the user has to check it.
    Check,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub title: &'static str,
    pub state: State,
    pub detail: String,
    pub fix: Option<&'static str>,
}

pub(crate) fn check(id: &'static str, title: &'static str, state: State, detail: impl Into<String>, fix: Option<&'static str>) -> Check {
    Check { id, title, state, detail: detail.into(), fix }
}

/// Spotify's prefs file; its keys are undocumented and may change between versions.
pub(crate) fn spotify_checks() -> Vec<Check> {
    let mut out = Vec::new();
    let prefs = spotify_prefs().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let value = |key: &str| prefs.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('='))).map(str::to_owned);

    match value("audio.play_bitrate_enumeration").and_then(|v| v.parse::<u32>().ok()) {
        Some(q) if q >= 5 => out.push(check("quality", "Streaming quality", State::Ok, "Lossless", None)),
        Some(_) => out.push(check("quality", "Streaming quality", State::Bad, "Not lossless. In Spotify: Settings → Audio quality → Streaming quality → Lossless.", None)),
        None => out.push(check("quality", "Streaming quality", State::Check, "Set Spotify → Settings → Audio quality → Streaming quality → Lossless.", None)),
    }
    out.push(match value("audio.allow_downgrade").as_deref() {
        Some("false") => check("downgrade", "Auto-adjust quality", State::Ok, "Off", None),
        _ => check("downgrade", "Auto-adjust quality", State::Warn, "Turn off \"Auto adjust quality\" so a slow network can't drop to lossy.", None),
    });
    out.push(match value("audio.normalize_v2").as_deref() {
        Some("false") => check("normalize", "Normalize volume", State::Ok, "Off", None),
        _ => check("normalize", "Normalize volume", State::Bad, "Turn off \"Normalize volume\" - it changes the level of every track.", None),
    });
    if let Some(device) = value("audio.device_selected").and_then(|v| serde_json::from_str::<String>(&v).ok()) {
        let exclusive = serde_json::from_str::<serde_json::Value>(&device).ok().and_then(|d| d["excl"].as_bool()).unwrap_or(false);
        out.push(if exclusive {
            check("exclusive", "Exclusive mode", State::Bad, "On - Spotify bypasses the Windows mixer, nothing can be captured. Turn it off.", None)
        } else {
            check("exclusive", "Exclusive mode", State::Ok, "Off", None)
        });
    }
    // Spotify only writes settings that differ from their default (off).
    out.push(match value("audio.equalizer_v2").as_deref() {
        Some("true") => check("equalizer", "Equalizer", State::Bad, "On - it changes every sample. In Spotify: Settings → Playback → Equalizer.", None),
        _ => check("equalizer", "Equalizer", State::Ok, "Off", None),
    });
    out.push(match value("audio.crossfade_v2").as_deref() {
        Some("true") => check("crossfade", "Crossfade", State::Bad, "On - track ends and starts get mixed. In Spotify: Settings → Playback → Crossfade.", None),
        _ => check("crossfade", "Crossfade", State::Ok, "Off", None),
    });
    out.push(check("automix", "Automix", State::Check, "Can't be read from Spotify - make sure Automix is off. Mitschnitt also detects altered audio in every file.", None));
    out
}

/// The most recently used `prefs` of the Spotify desktop app.
fn spotify_prefs() -> Option<PathBuf> {
    #[cfg(windows)]
    let roots = [
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Spotify").join("Users")),
        std::env::var_os("LOCALAPPDATA").map(|a| PathBuf::from(a).join(r"Packages\SpotifyAB.SpotifyMusic_zpdnekdrzrea0\LocalState\Spotify\Users")),
    ];
    // Native package, Flatpak, Snap.
    #[cfg(not(windows))]
    let roots = [
        dirs::config_dir().map(|c| c.join("spotify/Users")),
        dirs::home_dir().map(|h| h.join(".var/app/com.spotify.Client/config/spotify/Users")),
        dirs::home_dir().map(|h| h.join("snap/spotify/current/.config/spotify/Users")),
    ];
    roots
        .into_iter()
        .flatten()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flatten()
        .flatten()
        .map(|e| e.path().join("prefs"))
        .filter(|p| p.is_file())
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
}


//! Health checks of everything that affects a bit-exact recording, with one-click fixes where possible.

use std::path::PathBuf;

use serde::Serialize;

pub use crate::platform::os::setup::health;

use crate::profile::Profile;

/// Keys the checks read, and ones seen to change nothing (Spotify 1.3.4). Any other setting is unexpected: Spotify
/// renamed or moved one before (crossfade, 1.3.4), and the checks then read nothing.
const KNOWN_KEYS: &[&str] = &[
    "audio.play_bitrate_enumeration",
    "audio.allow_downgrade",
    "audio.normalize_v2",
    "audio.device_selected",
    "audio.equalizer_v2",
    "audio.crossfade_v2",
    "audio.crossfade_migrated_to_ps",
    "audio.crossfade.time_v2",
    "audio.automix",
    "audio.gapless_migrated_to_ps",
    "audio.play_bitrate_non_metered_migrated",
    "audio.play_bitrate_non_metered_enumeration",
];

pub fn fix(action: &str, profile: &Profile) -> Result<String, String> {
    match action {
        "spotify_keys" => {
            let keys = unexpected(&read_prefs(), &[]);
            std::fs::create_dir_all(crate::settings::state_dir()).map_err(|e| e.to_string())?;
            std::fs::write(acknowledged_path(), serde_json::to_string(&keys).unwrap_or_default()).map_err(|e| e.to_string())?;
            Ok("Noted. Mitschnitt warns again when Spotify's settings file changes.".into())
        }
        _ => crate::platform::os::setup::fix(action, profile),
    }
}

/// Audio settings and moved ones, neither known nor acknowledged.
fn unexpected(prefs: &str, acknowledged: &[String]) -> Vec<String> {
    prefs
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim()))
        .filter(|k| (k.starts_with("audio.") || k.contains("_migrated")) && !KNOWN_KEYS.contains(k) && !acknowledged.iter().any(|a| a == k))
        .map(str::to_owned)
        .collect()
}

fn acknowledged_path() -> PathBuf {
    crate::settings::state_dir().join("spotify-keys.json")
}

fn read_prefs() -> String {
    spotify_prefs().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default()
}

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
    let prefs = read_prefs();
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
        _ => check("normalize", "Normalize volume", State::Bad, "Turn off \"Normalize volume\" – it changes the level of every track.", None),
    });
    if let Some(device) = value("audio.device_selected").and_then(|v| serde_json::from_str::<String>(&v).ok()) {
        let exclusive = serde_json::from_str::<serde_json::Value>(&device).ok().and_then(|d| d["excl"].as_bool()).unwrap_or(false);
        out.push(if exclusive {
            check("exclusive", "Exclusive mode", State::Bad, "On – Spotify bypasses the Windows mixer, nothing can be captured. Turn it off.", None)
        } else {
            check("exclusive", "Exclusive mode", State::Ok, "Off", None)
        });
    }
    // Spotify only writes settings that differ from their default (off).
    out.push(match value("audio.equalizer_v2").as_deref() {
        Some("true") => check("equalizer", "Equalizer", State::Bad, "On – it changes every sample. In Spotify: Settings → Playback → Equalizer.", None),
        _ => check("equalizer", "Equalizer", State::Ok, "Off", None),
    });
    out.push(match (value("audio.crossfade_v2").as_deref(), value("audio.crossfade_migrated_to_ps")) {
        (Some("true"), _) => check("crossfade", "Crossfade", State::Bad, "On – track ends and starts get mixed. In Spotify: Settings → Playback → Crossfade.", None),
        // Spotify 1.3.4 keeps it outside the prefs file; Echolot runs still notice it in the audio.
        (None, Some(_)) => check("crossfade", "Crossfade", State::Check, "Make sure it is off – Spotify no longer shows it to Mitschnitt. In Spotify: Settings → Playback → Crossfade.", None),
        _ => check("crossfade", "Crossfade", State::Ok, "Off", None),
    });
    // On by default: only "off" is written (measured with 1.2.96).
    out.push(match value("audio.automix").as_deref() {
        Some("false") => check("automix", "Automix", State::Ok, "Off", None),
        _ => check("automix", "Automix", State::Warn, "On – it can blend song transitions. In Spotify: Settings → Playback → Automix.", None),
    });
    let acknowledged: Vec<String> = std::fs::read_to_string(acknowledged_path()).ok().and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
    let new = unexpected(&prefs, &acknowledged);
    if !new.is_empty() {
        let detail = format!("Spotify changed its settings file ({}): a setting may have moved where Mitschnitt can't read it. Check Spotify's Playback and Audio quality settings.", new.join(", "));
        out.push(check("spotify_keys", "Settings file", State::Warn, detail, Some("spotify_keys")));
    }
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


#[cfg(test)]
mod tests {
    #[test]
    fn unknown_audio_and_moved_settings_are_unexpected_until_acknowledged() {
        let prefs = "audio.normalize_v2=false\naudio.crossfade.time_v2=6000\nconnect.dial_devices=\"[]\"\naudio.loudness_v3=true\nui.track_notifications_migrated_to_ps=true\n";
        assert_eq!(super::unexpected(prefs, &[]), ["audio.loudness_v3", "ui.track_notifications_migrated_to_ps"]);
        assert_eq!(super::unexpected(prefs, &["audio.loudness_v3".into()]), ["ui.track_notifications_migrated_to_ps"]);
    }
}

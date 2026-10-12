//! User settings, persisted as JSON. Every field has a sensible default, so a missing or older file
//! simply falls back to defaults field by field.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Folder name under the system's config and state directories.
#[cfg(windows)]
pub const APP_DIR: &str = "Mitschnitt";
#[cfg(not(windows))]
pub const APP_DIR: &str = "mitschnitt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Flac,
    Wav,
    /// 320 kbit/s CBR, encoded from the verified recording (lossy - for devices that need it).
    Mp3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub out_dir: PathBuf,
    pub format: Format,
    /// File name without extension; `{artist}`, `{title}`, `{album}`; `/` creates folders.
    pub template: String,
    /// Keep files the forensic check marks as altered or corrupt (otherwise they are deleted).
    pub keep_flagged: bool,
    /// Don't save a track again when a file with its name already exists (any format).
    pub skip_existing: bool,
    /// Swallow media keys (keyboard, headset, earbuds) while a track is recording; only the player's own
    /// pause button can interrupt it.
    pub protect_media_keys: bool,
    /// Play the captured audio on the default output device (Windows only, see [`crate::monitor`]). Its volume
    /// is the app's Windows mixer entry (remembered by Windows), so it isn't stored here.
    pub monitor: bool,
    /// After Echolot answered for an upload, keep the recording in the output folder (otherwise it is deleted;
    /// files Echolot rejects are kept in "Not taken" either way).
    pub keep_echolot_copies: bool,
    /// The Echolot this PC was last connected to (the token itself is in the credential store).
    pub echolot_url: String,
}

impl Default for Settings {
    fn default() -> Self {
        let music = dirs::audio_dir().or_else(|| dirs::home_dir().map(|h| h.join("Music"))).unwrap_or_default();
        Settings {
            out_dir: music.join("Mitschnitt"),
            format: Format::Flac,
            template: "{artist} - {title}".into(),
            keep_flagged: true,
            skip_existing: true,
            protect_media_keys: true,
            monitor: true,
            keep_echolot_copies: true,
            echolot_url: String::new(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).expect("settings serialize"))
    }

    /// Where the settings live: `%APPDATA%\Mitschnitt\settings.json`, `~/.config/mitschnitt/settings.json`.
    pub fn default_path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(std::env::temp_dir).join(APP_DIR).join("settings.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_json_falls_back_to_defaults() {
        let s: Settings = serde_json::from_str(r#"{ "format": "wav" }"#).unwrap();
        assert_eq!(s.format, Format::Wav);
        assert_eq!(s.template, "{artist} - {title}");
    }
}

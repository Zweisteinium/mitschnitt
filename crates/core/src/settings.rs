//! User settings as JSON; missing fields fall back to their defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(windows)]
pub const APP_DIR: &str = "Mitschnitt";
#[cfg(not(windows))]
pub const APP_DIR: &str = "mitschnitt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Flac,
    Wav,
    /// 320 kbit/s CBR.
    Mp3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub out_dir: PathBuf,
    pub format: Format,
    /// File name without extension; `{artist}`, `{title}`, `{album}`; `/` creates folders.
    pub template: String,
    /// Keep files the forensic check marks as altered or corrupt.
    pub keep_flagged: bool,
    /// Skip a track when a file with its name exists (any format).
    pub skip_existing: bool,
    pub protect_media_keys: bool,
    /// Windows only. Its volume is the app's Windows mixer entry, which Windows remembers, so it isn't stored here.
    pub monitor: bool,
    /// Keep a recording in the output folder after Echolot took it.
    pub keep_echolot_copies: bool,
    /// The token is in the credential store.
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

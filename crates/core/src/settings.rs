//! User settings as JSON; missing fields fall back to their defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(windows)]
pub const APP_DIR: &str = "Mitschnitt";
#[cfg(not(windows))]
pub const APP_DIR: &str = "mitschnitt";

/// Local, not roaming: what belongs to this PC (restore state, log).
pub fn state_dir() -> PathBuf {
    dirs::state_dir().or_else(dirs::data_local_dir).unwrap_or_else(std::env::temp_dir).join(APP_DIR)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// Earlier settings' WAV: lossless too, and with tags and a cover.
    #[serde(alias = "wav")]
    Flac,
    /// Apple Lossless in .m4a: Windows' own encoder, on Linux Mitschnitt's.
    Alac,
    /// 256 kbit/s VBR, at 48 kHz (Opus always is).
    Opus,
    /// 192 kbit/s in .m4a: Windows' own encoder (it goes no higher), on Linux ffmpeg's.
    Aac,
    /// 320 kbit/s CBR.
    Mp3,
}

impl Format {
    pub fn ext(self) -> &'static str {
        match self {
            Format::Flac => "flac",
            Format::Alac | Format::Aac => "m4a",
            Format::Opus => "opus",
            Format::Mp3 => "mp3",
        }
    }

    pub fn lossless(self) -> bool {
        matches!(self, Format::Flac | Format::Alac)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub out_dir: PathBuf,
    /// For songs that arrive bit-exact (earlier settings had one `format` for all).
    #[serde(alias = "format")]
    pub lossless_format: Format,
    /// For lossy streams: a lossy format encodes them a second time.
    pub lossy_format: Format,
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
            lossless_format: Format::Flac,
            lossy_format: Format::Opus,
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
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str::<Settings>(&s).ok()).map(Settings::migrated).unwrap_or_default()
    }

    /// One format for all (earlier versions) that was lossy: it becomes the lossy songs' one, the exact ones get FLAC.
    fn migrated(self) -> Settings {
        if self.lossless_format.lossless() {
            return self;
        }
        Settings { lossy_format: self.lossless_format, lossless_format: Format::Flac, ..self }
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
        assert_eq!((s.lossless_format, s.lossy_format), (Format::Flac, Format::Opus), "WAV is gone, the old field still counts");
        assert_eq!(s.template, "{artist} - {title}");
        let mp3: Settings = serde_json::from_str::<Settings>(r#"{ "format": "mp3" }"#).unwrap().migrated();
        assert_eq!((mp3.lossless_format, mp3.lossy_format), (Format::Flac, Format::Mp3));
    }
}

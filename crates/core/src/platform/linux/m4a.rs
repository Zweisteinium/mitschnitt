//! .m4a (AAC, ALAC) uses Windows' own encoders; Linux has none that may ship with Mitschnitt.

use crate::audio::Audio;

pub const SUPPORTED: bool = false;

pub fn write(_path: &std::path::Path, _audio: &Audio, _alac: bool) -> Result<(), String> {
    Err("AAC and ALAC need Windows: pick FLAC, Opus or MP3".into())
}

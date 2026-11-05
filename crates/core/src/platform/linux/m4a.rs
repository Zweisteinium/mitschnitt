//! .m4a: ALAC by Mitschnitt's own encoder, AAC by the system's ffmpeg, where it is installed. No AAC encoder may ship
//! with Mitschnitt: the good one (FDK) has a license that doesn't go with the AGPL.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::audio::Audio;

/// Like Windows' encoder, which goes no higher.
const AAC_BITRATE: &str = "192k";

/// ALAC always; AAC when ffmpeg is installed with its AAC encoder (asked each time: it may come later).
pub fn supports(alac: bool) -> bool {
    alac || Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).stdin(Stdio::null()).stderr(Stdio::null()).output().is_ok_and(|out| has_aac(&String::from_utf8_lossy(&out.stdout)))
}

/// " A....D aac                  AAC (Advanced Audio Coding)", among the others.
fn has_aac(encoders: &str) -> bool {
    encoders.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next().is_some_and(|flags| flags.starts_with('A')) && words.next() == Some("aac")
    })
}

pub fn write(path: &Path, audio: &Audio, alac: bool) -> Result<(), String> {
    if alac { crate::alac::write(path, audio) } else { aac(path, audio).map_err(|e| format!("AAC (ffmpeg): {e}")) }
}

/// The full resolution goes in, left-justified in 32 bits.
fn aac(path: &Path, audio: &Audio) -> Result<(), String> {
    let (rate, channels) = (audio.rate.to_string(), audio.channels.to_string());
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "s32le", "-ar", &rate, "-ac", &channels, "-i", "pipe:0", "-c:a", "aac", "-b:a", AAC_BITRATE, "-f", "ipod"])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { "not installed".into() } else { e.to_string() })?;
    let shift = 32 - audio.bits;
    let pcm: Vec<u8> = audio.samples.iter().flat_map(|s| (s << shift).to_le_bytes()).collect();
    // Closed when dropped, which ends ffmpeg's input.
    let fed = child.stdin.take().ok_or("no input")?.write_all(&pcm);
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    fed.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpegs_aac_encoder_is_found_in_its_list() {
        let list = " V....D libx264              libx264 H.264\n A....D aac                  AAC (Advanced Audio Coding)\n";
        assert!(has_aac(list));
        assert!(!has_aac(" A....D alac                 ALAC (Apple Lossless Audio Codec)\n A....D libfdk_aac           Fraunhofer\n"));
    }

    #[test]
    fn aac_is_written_by_ffmpeg_at_the_level_it_came_in() {
        if !supports(false) {
            return eprintln!("no ffmpeg with AAC, skipped");
        }
        let audio = Audio { rate: 44_100, channels: 2, bits: 24, samples: (0..88_200).map(|i| ((i / 2) as f64 * 0.05).sin()).map(|v| (v * 4e6) as i32).collect() };
        let path = std::env::temp_dir().join(format!("mitschnitt-aac-{}.m4a", std::process::id()));
        write(&path, &audio, false).unwrap();
        let (rate, channels, _, samples) = crate::alac::tests::decode(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!((rate, channels), (44_100, 2));
        let rms = |x: &[f64]| (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt();
        let level = rms(&samples.iter().map(|s| *s as f64).collect::<Vec<_>>()) / rms(&audio.samples.iter().map(|s| *s as f64 / 8_388_608.0 * 2_147_483_648.0).collect::<Vec<_>>());
        assert!((level - 1.0).abs() < 0.05, "the same level after encoding: {level}");
    }
}

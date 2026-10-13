//! Writing a finished recording according to the settings.

use std::path::{Path, PathBuf};

use crate::audio::Audio;
use crate::flac::{self, Tags};
use crate::settings::{Format, Settings};

const EXTENSIONS: [&str; 3] = ["flac", "wav", "mp3"];

/// A recording of this track with the same name, in any format.
pub fn existing(tags: &Tags, settings: &Settings) -> Option<PathBuf> {
    let base = settings.out_dir.join(file_stem(&settings.template, tags));
    EXTENSIONS.iter().map(|ext| with_ext(&base, ext)).find(|p| p.exists())
}

pub fn write(audio: &Audio, tags: &Tags, settings: &Settings) -> Result<PathBuf, String> {
    let ext = match settings.format {
        Format::Flac => "flac",
        Format::Wav => "wav",
        Format::Mp3 => "mp3",
    };
    let path = unique_path(&settings.out_dir, &file_stem(&settings.template, tags), ext);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("folder {}: {e}", dir.display()))?;
    }
    // 16 bit when every sample lies on the 16-bit grid (a 16-bit master): lossless and smaller. Otherwise 24 bit.
    let out = is_16bit(audio).then(|| to_16bit(audio));
    match settings.format {
        Format::Flac => flac::write(&path, out.as_ref().unwrap_or(audio), tags)?,
        Format::Wav => write_wav(&path, out.as_ref().unwrap_or(audio))?,
        Format::Mp3 => write_mp3(&path, audio, tags)?,
    }
    Ok(path)
}

/// From the full-resolution samples.
fn write_mp3(path: &Path, audio: &Audio, tags: &Tags) -> Result<(), String> {
    use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, InterleavedPcm, Quality};
    if audio.channels != 2 {
        return Err(format!("mp3: {} channels (stereo only)", audio.channels));
    }
    let err = |e: &dyn std::fmt::Debug| format!("mp3: {e:?}");
    let mut builder = Builder::new().ok_or("mp3: encoder unavailable")?;
    builder.set_num_channels(2).map_err(|e| err(&e))?;
    builder.set_sample_rate(audio.rate).map_err(|e| err(&e))?;
    builder.set_brate(Bitrate::Kbps320).map_err(|e| err(&e))?;
    builder.set_quality(Quality::Best).map_err(|e| err(&e))?;
    let mut encoder = builder.build().map_err(|e| err(&e))?;

    let scale = 1.0 / (1u64 << (audio.bits - 1)) as f32;
    let pcm: Vec<f32> = audio.samples.iter().map(|s| *s as f32 * scale).collect();
    // encode_to_vec only fills spare capacity; LAME's worst case is 1.25 x samples + 7200 bytes.
    let mut data = Vec::new();
    for chunk in pcm.chunks(2 * 44_100) {
        data.reserve(chunk.len() * 5 / 8 + 7200);
        encoder.encode_to_vec(InterleavedPcm(chunk), &mut data).map_err(|e| err(&e))?;
    }
    data.reserve(7200);
    encoder.flush_to_vec::<FlushNoGap>(&mut data).map_err(|e| err(&e))?;

    via_part(path, "mp3.part", |part| {
        std::fs::write(part, &data).map_err(|e| format!("write {}: {e}", part.display()))?;
        id3_tags(tags).write_to_path(part, id3::Version::Id3v24).map_err(|e| format!("tags: {e}"))
    })
}

/// `write` creates the file under a temporary extension; it is renamed to `path` on success, removed otherwise.
pub(crate) fn via_part(path: &Path, ext: &str, write: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
    let part = path.with_extension(ext);
    let result = write(&part).and_then(|()| std::fs::rename(&part, path).map_err(|e| format!("rename: {e}")));
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

fn id3_tags(tags: &Tags) -> id3::Tag {
    use id3::TagLike;
    let mut tag = id3::Tag::new();
    tag.set_title(&tags.title);
    tag.set_artist(&tags.artist);
    if !tags.album.is_empty() {
        tag.set_album(&tags.album);
    }
    if !tags.album_artist.is_empty() {
        tag.set_album_artist(&tags.album_artist);
    }
    if tags.track_number > 0 {
        tag.set_track(tags.track_number);
    }
    if let Some(cover) = &tags.cover {
        tag.add_frame(id3::frame::Picture {
            mime_type: cover.mime.to_owned(),
            picture_type: id3::frame::PictureType::CoverFront,
            description: String::new(),
            data: cover.data.clone(),
        });
    }
    tag.set_text("TSSE", "Mitschnitt");
    tag
}

/// The 24-bit capture holds a 16-bit master exactly.
pub fn is_16bit(audio: &Audio) -> bool {
    audio.bits <= 16 || audio.samples.iter().all(|s| s % (1 << (audio.bits - 16)) == 0)
}

/// Rounds to nearest; exact when [`is_16bit`].
fn to_16bit(audio: &Audio) -> Audio {
    let shift = audio.bits.saturating_sub(16);
    let half = if shift > 0 { 1i64 << (shift - 1) } else { 0 };
    let samples = audio.samples.iter().map(|s| ((*s as i64 + half) >> shift).clamp(-32_768, 32_767) as i32).collect();
    Audio { rate: audio.rate, channels: audio.channels, bits: 16, samples }
}

fn write_wav(path: &Path, audio: &Audio) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: audio.channels as u16,
        sample_rate: audio.rate,
        bits_per_sample: audio.bits as u16,
        sample_format: hound::SampleFormat::Int,
    };
    via_part(path, "wav.part", |part| {
        let mut writer = hound::WavWriter::create(part, spec).map_err(|e| format!("wav: {e}"))?;
        for s in &audio.samples {
            writer.write_sample(*s).map_err(|e| format!("wav: {e}"))?;
        }
        writer.finalize().map_err(|e| format!("wav: {e}"))?;
        let back: Result<Vec<i32>, _> = hound::WavReader::open(part).map_err(|e| format!("verify: {e}"))?.samples::<i32>().collect();
        if back.map_err(|e| format!("verify: {e}"))? != audio.samples {
            return Err("verify: written WAV differs from the recording".to_owned());
        }
        Ok(())
    })
}

/// Only `/` in the template creates folders, so "AC/DC" stays one name.
pub fn file_stem(template: &str, tags: &Tags) -> PathBuf {
    let parts: Vec<String> = template
        .split(['/', '\\'])
        .map(|part| sanitize(&part.replace("{artist}", &tags.artist).replace("{title}", &tags.title).replace("{album}", &tags.album)))
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() { PathBuf::from(sanitize(&tags.title)) } else { parts.iter().collect() }
}

fn sanitize(part: &str) -> String {
    let s: String = part.chars().map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    s.trim().trim_end_matches('.').trim().to_owned()
}

fn unique_path(dir: &Path, stem: &Path, ext: &str) -> PathBuf {
    let base = dir.join(stem);
    let mut path = with_ext(&base, ext);
    let mut n = 2;
    while path.exists() {
        path = with_ext(&PathBuf::from(format!("{} ({n})", base.display())), ext);
        n += 1;
    }
    path
}

/// Appended, not `with_extension`: that took "Abr. - Hell's River" for a name with the extension " - Hell's River".
fn with_ext(base: &Path, ext: &str) -> PathBuf {
    let mut name = base.as_os_str().to_owned();
    name.push(format!(".{ext}"));
    name.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Tags {
        Tags { title: "What? Now".into(), artist: "AC/DC".into(), album: "Live: 1991".into(), ..Tags::default() }
    }

    #[test]
    fn template_makes_safe_folders_and_names() {
        assert_eq!(file_stem("{artist} - {title}", &track()), PathBuf::from("AC_DC - What_ Now"));
        assert_eq!(file_stem("{album}/{title}", &track()), PathBuf::from("Live_ 1991").join("What_ Now"));
    }

    #[test]
    fn auto_bit_depth_writes_16_bit_only_when_lossless() {
        let a16 = Audio { rate: 44_100, channels: 2, bits: 24, samples: vec![256, -512, 0, 32_767 << 8] };
        assert!(is_16bit(&a16));
        assert_eq!(to_16bit(&a16).samples, vec![1, -2, 0, 32_767]);
        let a24 = Audio { samples: vec![257, 0, 0, 0], ..a16 };
        assert!(!is_16bit(&a24));
    }

    #[test]
    fn wav_round_trip_is_exact() {
        let audio = Audio { rate: 44_100, channels: 2, bits: 24, samples: (0..20_000).map(|i| (i * 977 % 8_000_000) - 4_000_000).collect() };
        let dir = std::env::temp_dir().join(format!("mitschnitt-wav-{}", std::process::id()));
        let settings = Settings { out_dir: dir.clone(), format: Format::Wav, ..Settings::default() };
        let path = write(&audio, &track(), &settings).unwrap();
        let back: Vec<i32> = hound::WavReader::open(&path).unwrap().samples::<i32>().map(Result::unwrap).collect();
        assert_eq!(back, audio.samples);
        assert_eq!(existing(&track(), &settings), Some(path));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_dot_in_the_name_is_kept() {
        let dir = std::env::temp_dir().join(format!("mitschnitt-dot-{}", std::process::id()));
        let settings = Settings { out_dir: dir.clone(), format: Format::Wav, ..Settings::default() };
        let tags = Tags { title: "Hell's River".into(), artist: "Abr.".into(), ..Tags::default() };
        let audio = Audio { rate: 44_100, channels: 2, bits: 16, samples: vec![0; 4] };
        let names: Vec<_> = (0..2).map(|_| write(&audio, &tags, &settings).unwrap().file_name().unwrap().to_owned()).collect();
        assert_eq!(names, ["Abr. - Hell's River.wav", "Abr. - Hell's River (2).wav"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mp3_is_written_with_tags() {
        let audio = Audio { rate: 44_100, channels: 2, bits: 24, samples: (0..88_200).map(|i| ((i as f64 * 0.05).sin() * 3e6) as i32).collect() };
        let dir = std::env::temp_dir().join(format!("mitschnitt-mp3-{}", std::process::id()));
        let settings = Settings { out_dir: dir.clone(), format: Format::Mp3, ..Settings::default() };
        let tags = Tags { track_number: 3, cover: Some(flac::Cover { mime: "image/png", data: vec![0x89, b'P', b'N', b'G'] }), ..track() };
        let path = write(&audio, &tags, &settings).unwrap();
        let tag = id3::Tag::read_from_path(&path).unwrap();
        use id3::TagLike;
        assert_eq!((tag.artist(), tag.track(), tag.pictures().count()), (Some("AC/DC"), Some(3), 1));
        assert!(std::fs::metadata(&path).unwrap().len() > 20_000, "about 40 kB for 1 s at 320 kbit/s");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

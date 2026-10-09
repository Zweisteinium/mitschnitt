//! Writing a finished recording according to the settings: file name from the template, format, bit depth.
//! Lossless files are read back and compared before they get their final name.

use std::path::{Path, PathBuf};

use crate::audio::Audio;
use crate::flac::{self, Tags};
use crate::settings::{BitDepth, Format, Settings};

const EXTENSIONS: [&str; 3] = ["flac", "wav", "mp3"];

/// An existing recording of this track (same name, any format), if any.
pub fn existing(tags: &Tags, settings: &Settings) -> Option<PathBuf> {
    let base = settings.out_dir.join(file_stem(&settings.template, tags));
    EXTENSIONS.iter().map(|ext| base.with_extension(ext)).find(|p| p.exists())
}

/// Write `audio` (24-bit) with `tags`; returns the final path.
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
    let out = match settings.bit_depth {
        BitDepth::B24 => None,
        BitDepth::B16 => Some(to_16bit(audio)),
        BitDepth::Auto => is_16bit(audio).then(|| to_16bit(audio)),
    };
    match settings.format {
        Format::Flac => flac::write(&path, out.as_ref().unwrap_or(audio), tags)?,
        Format::Wav => write_wav(&path, out.as_ref().unwrap_or(audio))?,
        Format::Mp3 => write_mp3(&path, audio, tags)?,
    }
    Ok(path)
}

/// 320 kbit/s CBR from the full-resolution samples, ID3v2.4 tags with cover.
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
    let mut data = Vec::with_capacity(pcm.len() * 5 / 8 + 7200);
    for chunk in pcm.chunks(2 * 44_100) {
        data.reserve(chunk.len() * 5 / 8 + 7200);
        encoder.encode_to_vec(InterleavedPcm(chunk), &mut data).map_err(|e| err(&e))?;
    }
    data.reserve(7200);
    encoder.flush_to_vec::<FlushNoGap>(&mut data).map_err(|e| err(&e))?;

    let part = path.with_extension("mp3.part");
    let result = (|| {
        std::fs::write(&part, &data).map_err(|e| format!("write {}: {e}", part.display()))?;
        id3_tags(tags).write_to_path(&part, id3::Version::Id3v24).map_err(|e| format!("tags: {e}"))?;
        std::fs::rename(&part, path).map_err(|e| format!("rename: {e}"))
    })();
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

/// Every sample on the 16-bit grid: the 24-bit capture holds a 16-bit master exactly.
pub fn is_16bit(audio: &Audio) -> bool {
    audio.bits <= 16 || audio.samples.iter().all(|s| s % (1 << (audio.bits - 16)) == 0)
}

/// Reduce to 16 bit (round to nearest; exact when [`is_16bit`]).
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
    let part = path.with_extension("wav.part");
    let result = (|| {
        let mut writer = hound::WavWriter::create(&part, spec).map_err(|e| format!("wav: {e}"))?;
        for s in &audio.samples {
            writer.write_sample(*s).map_err(|e| format!("wav: {e}"))?;
        }
        writer.finalize().map_err(|e| format!("wav: {e}"))?;
        let back: Result<Vec<i32>, _> = hound::WavReader::open(&part).map_err(|e| format!("verify: {e}"))?.samples::<i32>().collect();
        if back.map_err(|e| format!("verify: {e}"))? != audio.samples {
            return Err("verify: written WAV differs from the recording".to_owned());
        }
        std::fs::rename(&part, path).map_err(|e| format!("rename: {e}"))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

/// File stem from the template. Only `/` in the *template* creates folders; values are filled per part and
/// sanitised, so "AC/DC" stays one name.
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
    let mut path = base.with_extension(ext);
    let mut n = 2;
    while path.exists() {
        let name = format!("{} ({n})", base.file_name().unwrap_or_default().to_string_lossy());
        path = base.with_file_name(name).with_extension(ext);
        n += 1;
    }
    path
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
        let settings = Settings { out_dir: dir.clone(), format: Format::Wav, bit_depth: BitDepth::Auto, ..Settings::default() };
        let path = write(&audio, &track(), &settings).unwrap();
        let back: Vec<i32> = hound::WavReader::open(&path).unwrap().samples::<i32>().map(Result::unwrap).collect();
        assert_eq!(back, audio.samples);
        assert_eq!(existing(&track(), &settings), Some(path));
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

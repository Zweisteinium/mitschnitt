//! Writing a finished recording according to the settings.

use std::path::{Path, PathBuf};

use crate::audio::Audio;
use crate::flac::{self, Tags};
use crate::settings::{Format, Settings};

/// WAV: written by earlier versions.
const EXTENSIONS: [&str; 5] = ["flac", "m4a", "opus", "mp3", "wav"];

/// A recording of this track with the same name, in any format.
pub fn existing(tags: &Tags, settings: &Settings) -> Option<PathBuf> {
    let base = settings.out_dir.join(file_stem(&settings.template, tags));
    EXTENSIONS.iter().map(|ext| with_ext(&base, ext)).find(|p| p.exists())
}

pub fn write(audio: &Audio, tags: &Tags, settings: &Settings, format: Format) -> Result<PathBuf, String> {
    let path = unique_path(&settings.out_dir, &file_stem(&settings.template, tags), format.ext());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("folder {}: {e}", dir.display()))?;
    }
    // 16 bit when every sample lies on the 16-bit grid (a 16-bit master): lossless and smaller. Otherwise 24 bit.
    let out = is_16bit(audio).then(|| to_16bit(audio));
    let exact = out.as_ref().unwrap_or(audio);
    match format {
        Format::Flac => flac::write(&path, exact, tags)?,
        Format::Alac | Format::Aac => via_part(&path, "m4a.part", |part| {
            crate::platform::os::m4a::write(part, exact, format == Format::Alac)?;
            m4a_tags(part, tags)
        })?,
        Format::Opus => crate::ogg_opus::write(&path, audio, tags)?,
        Format::Mp3 => write_mp3(&path, audio, tags)?,
    }
    Ok(path)
}

/// iTunes-style tags, written into the finished file.
fn m4a_tags(path: &Path, tags: &Tags) -> Result<(), String> {
    use mp4ameta::{Data, FreeformIdent, Img};
    let mut tag = mp4ameta::Tag::read_from_path(path).map_err(|e| format!("m4a tags: {e}"))?;
    tag.set_title(&tags.title);
    tag.set_artist(&tags.artist);
    if !tags.album.is_empty() {
        tag.set_album(&tags.album);
    }
    if !tags.album_artist.is_empty() {
        tag.set_album_artist(&tags.album_artist);
    }
    if tags.track_number > 0 {
        tag.set_track_number(tags.track_number as u16);
    }
    if let Some(cover) = &tags.cover {
        tag.set_artwork(if cover.mime == "image/png" { Img::png(cover.data.clone()) } else { Img::jpeg(cover.data.clone()) });
    }
    for (key, value) in &tags.extra {
        tag.set_data(FreeformIdent::new_borrowed("com.apple.iTunes", key), Data::Utf8(value.clone()));
    }
    tag.set_encoder("Mitschnitt");
    tag.write_to_path(path).map_err(|e| format!("m4a tags: {e}"))
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

/// An image for the window: `data:<mime>;base64,…`.
pub fn data_url(mime: &str, data: &[u8]) -> String {
    use base64::Engine as _;
    format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(data))
}

/// The cover a recording carries, read from its tags only.
pub fn cover_url(path: &Path) -> Option<String> {
    match path.extension()?.to_str()? {
        "mp3" => {
            let tag = id3::Tag::read_from_path(path).ok()?;
            let picture = tag.pictures().next()?;
            Some(data_url(&picture.mime_type, &picture.data))
        }
        "m4a" => {
            let tag = mp4ameta::Tag::read_from_path(path).ok()?;
            let image = tag.artwork()?;
            Some(data_url(if image.fmt == mp4ameta::ImgFmt::Png { "image/png" } else { "image/jpeg" }, image.data))
        }
        "opus" => crate::ogg_opus::cover(path).map(|(mime, data)| data_url(&mime, &data)),
        _ => {
            let picture = flac_codec::metadata::block::<_, flac_codec::metadata::Picture>(path).ok()??;
            Some(data_url(&picture.media_type, &picture.data))
        }
    }
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
    fn a_dot_in_the_name_is_kept() {
        let dir = std::env::temp_dir().join(format!("mitschnitt-dot-{}", std::process::id()));
        let settings = Settings { out_dir: dir.clone(), ..Settings::default() };
        let tags = Tags { title: "Hell's River".into(), artist: "Abr.".into(), ..Tags::default() };
        let audio = Audio { rate: 44_100, channels: 2, bits: 16, samples: vec![0; 4] };
        let names: Vec<_> = (0..2).map(|_| write(&audio, &tags, &settings, Format::Flac).unwrap().file_name().unwrap().to_owned()).collect();
        assert_eq!(names, ["Abr. - Hell's River.flac", "Abr. - Hell's River (2).flac"]);
        assert_eq!(existing(&tags, &settings), Some(dir.join("Abr. - Hell's River.flac")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_flacs_cover_is_read_back_from_its_metadata() {
        let dir = std::env::temp_dir().join(format!("mitschnitt-cover-{}", std::process::id()));
        let settings = Settings { out_dir: dir.clone(), ..Settings::default() };
        let jpeg = JPEG.to_vec();
        let tags = Tags { cover: Some(flac::Cover { mime: "image/jpeg", data: jpeg.clone() }), ..track() };
        let audio = Audio { rate: 44_100, channels: 2, bits: 24, samples: vec![256; 8_820] };
        let path = write(&audio, &tags, &settings, Format::Flac).unwrap();
        assert_eq!(cover_url(&path), Some(data_url("image/jpeg", &jpeg)));
        let bare = write(&audio, &Tags { cover: None, ..tags }, &settings, Format::Flac).unwrap();
        assert_eq!(cover_url(&bare), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The smallest JPEG header that names its size: the writers leave out an image they can't measure.
    const JPEG: [u8; 21] = [0xFF, 0xD8, 0xFF, 0xC0, 0, 17, 8, 0, 64, 0, 64, 3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1];

    fn second_of_sound(bits: u32) -> Audio {
        let samples = (0..88_200).map(|i| ((i as f64 * 0.05).sin() * 3e6) as i32 >> (24 - bits)).collect();
        Audio { rate: 44_100, channels: 2, bits, samples }
    }

    #[test]
    fn opus_ends_where_the_song_ends_and_carries_its_cover() {
        let dir = std::env::temp_dir().join(format!("mitschnitt-opus-{}", std::process::id()));
        let settings = Settings { out_dir: dir.clone(), ..Settings::default() };
        let tags = Tags { cover: Some(flac::Cover { mime: "image/jpeg", data: JPEG.to_vec() }), ..track() };
        let path = write(&second_of_sound(24), &tags, &settings, Format::Opus).unwrap();
        let mut reader = ogg::PacketReader::new(std::fs::File::open(&path).unwrap());
        let (mut end, mut decoded) = (0, Vec::new());
        let mut decoder = opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap();
        while let Some(packet) = reader.read_packet().unwrap() {
            if !packet.data.starts_with(b"Opus") {
                let mut frame = [0.0f32; 960 * 2];
                let n = decoder.decode_float(&packet.data, &mut frame, false).unwrap();
                decoded.extend_from_slice(&frame[..n * 2]);
            }
            end = packet.absgp_page();
        }
        let pre_skip = { let head = std::fs::read(&path).unwrap(); let at = head.windows(8).position(|w| w == b"OpusHead").unwrap(); u16::from_le_bytes([head[at + 10], head[at + 11]]) as u64 };
        assert_eq!(end - pre_skip, 48_000, "1 s at 48 kHz, the padding of the last frame cut");
        assert!(decoded.len() / 2 >= (pre_skip + 48_000) as usize, "the song's end is encoded too: {} frames", decoded.len() / 2);
        let rms = |x: &[f32]| (x.iter().map(|v| (v * v) as f64).sum::<f64>() / x.len() as f64).sqrt();
        let input: Vec<f32> = second_of_sound(24).samples.iter().map(|s| *s as f32 / 8_388_608.0).collect();
        let level = rms(&decoded[pre_skip as usize * 2..]) / rms(&input);
        assert!((level - 1.0).abs() < 0.05, "the same level after encoding: {level}");
        assert_eq!(cover_url(&path), Some(data_url("image/jpeg", &JPEG)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn m4a_holds_aac_or_alac_with_tags_and_cover() {
        let dir = std::env::temp_dir().join(format!("mitschnitt-m4a-{}", std::process::id()));
        let settings = Settings { out_dir: dir.clone(), ..Settings::default() };
        let tags = Tags { track_number: 3, cover: Some(flac::Cover { mime: "image/jpeg", data: JPEG.to_vec() }), extra: vec![("ISRC".into(), "DEUM72401234".into())], ..track() };
        for (format, bits) in [(Format::Aac, 24), (Format::Alac, 24), (Format::Alac, 16)] {
            let path = write(&second_of_sound(bits), &tags, &settings, format).unwrap();
            let tag = mp4ameta::Tag::read_from_path(&path).unwrap();
            assert_eq!((tag.title(), tag.artist(), tag.track_number()), (Some("What? Now"), Some("AC/DC"), Some(3)), "{format:?}");
            assert!((tag.info.duration.as_secs_f64() - 1.0).abs() < 0.05, "{format:?}: {:?}", tag.info.duration);
            assert_eq!(cover_url(&path), Some(data_url("image/jpeg", &JPEG)), "{format:?}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mp3_is_written_with_tags() {
        let audio = Audio { rate: 44_100, channels: 2, bits: 24, samples: (0..88_200).map(|i| ((i as f64 * 0.05).sin() * 3e6) as i32).collect() };
        let dir = std::env::temp_dir().join(format!("mitschnitt-mp3-{}", std::process::id()));
        let settings = Settings { out_dir: dir.clone(), ..Settings::default() };
        let tags = Tags { track_number: 3, cover: Some(flac::Cover { mime: "image/png", data: vec![0x89, b'P', b'N', b'G'] }), ..track() };
        let path = write(&audio, &tags, &settings, Format::Mp3).unwrap();
        let tag = id3::Tag::read_from_path(&path).unwrap();
        use id3::TagLike;
        assert_eq!((tag.artist(), tag.track(), tag.pictures().count()), (Some("AC/DC"), Some(3), 1));
        assert_eq!(cover_url(&path).as_deref(), Some("data:image/png;base64,iVBORw=="));
        assert!(std::fs::metadata(&path).unwrap().len() > 20_000, "about 40 kB for 1 s at 320 kbit/s");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

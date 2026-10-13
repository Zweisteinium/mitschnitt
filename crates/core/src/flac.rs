//! FLAC files: reading into [`Audio`], and writing a recording as a verified FLAC.

use std::path::Path;

use flac_codec::decode::FlacSampleReader;
use flac_codec::encode::{FlacSampleWriter, Options};
use flac_codec::metadata::{Metadata, Picture, PictureType};

use crate::audio::Audio;

#[derive(Debug, Default, Clone)]
pub struct Tags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    /// 0 = unknown.
    pub track_number: u32,
    pub cover: Option<Cover>,
    /// More Vorbis comments, e.g. `ISRC`.
    pub extra: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Cover {
    pub mime: &'static str,
    pub data: Vec<u8>,
}

pub fn read(path: &Path) -> Result<Audio, String> {
    let mut reader = FlacSampleReader::open(path).map_err(|e| e.to_string())?;
    let mut samples = Vec::new();
    reader.read_to_end(&mut samples).map_err(|e| e.to_string())?;
    Ok(Audio { rate: reader.sample_rate(), channels: reader.channel_count().into(), bits: reader.bits_per_sample(), samples })
}

/// The file is decoded again and compared; only an identical file gets its final name.
pub fn write(path: &Path, audio: &Audio, tags: &Tags) -> Result<(), String> {
    let mut options = Options::best().overwrite();
    let fields = [("TITLE", &tags.title), ("ARTIST", &tags.artist), ("ALBUM", &tags.album), ("ALBUMARTIST", &tags.album_artist)];
    for (key, value) in fields.into_iter().chain(tags.extra.iter().map(|(k, v)| (k.as_str(), v))) {
        if !value.is_empty() {
            options = options.tag(key, value);
        }
    }
    if tags.track_number > 0 {
        options = options.tag("TRACKNUMBER", tags.track_number);
    }
    options = options.tag("ENCODER", "Mitschnitt");
    // An image it can't measure would cost the whole recording: then the file goes without a cover.
    if let Some(picture) = tags.cover.as_ref().and_then(|c| Picture::new(PictureType::FrontCover, "", c.data.clone()).ok()) {
        options = options.picture(picture);
    }

    crate::output::via_part(path, "flac.part", |part| {
        let err = |e: flac_codec::Error| format!("encode: {e}");
        let mut writer = FlacSampleWriter::create(part, options, audio.rate, audio.bits, audio.channels as u8, Some(audio.samples.len() as u64)).map_err(err)?;
        writer.write(&audio.samples).map_err(err)?;
        writer.finalize().map_err(err)?;
        let back = read(part).map_err(|e| format!("verify: {e}"))?;
        if back.samples != audio.samples || back.rate != audio.rate || back.channels != audio.channels {
            return Err("verify: decoded file differs from the recording".to_owned());
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flac_codec::metadata::{VorbisComment, blocks_of};

    #[test]
    fn write_then_read_is_bit_identical_and_tagged() {
        let samples: Vec<i32> = (0..44_100 * 2).map(|i| ((i * 7919) % 16_000_000) - 8_000_000).collect();
        let audio = Audio { rate: 44_100, channels: 2, bits: 24, samples };
        let path = std::env::temp_dir().join(format!("mitschnitt-test-{}.flac", std::process::id()));
        // The smallest JPEG header that names its size.
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0, 17, 8, 0, 64, 0, 64, 3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1];
        let tags = Tags { title: "Title".into(), artist: "Ärtist".into(), track_number: 7, cover: Some(Cover { mime: "image/jpeg", data: jpeg }), extra: vec![("ISRC".into(), "DEUM72401234".into())], ..Tags::default() };
        write(&path, &audio, &tags).unwrap();
        assert_eq!(read(&path).unwrap().samples, audio.samples);
        let comment = flac_codec::metadata::block::<_, VorbisComment>(&path).unwrap().unwrap();
        assert_eq!([comment.get("ARTIST"), comment.get("TRACKNUMBER"), comment.get("ISRC")], [Some("Ärtist"), Some("7"), Some("DEUM72401234")]);
        let covers: Vec<Picture> = blocks_of(&path).collect::<Result<_, _>>().unwrap();
        assert_eq!(covers.iter().map(|p| (p.width, p.media_type.as_str())).collect::<Vec<_>>(), [(64, "image/jpeg")]);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn sixteen_bit_audio_in_a_24_bit_file_costs_no_more_than_16_bit() {
        // A recording with the player's fade-in is 24-bit, the rest 16-bit audio: flacenc stored the 8 empty bits.
        let samples: Vec<i32> = (0..44_100 * 2).map(|i| (((i as f64 * 0.01).sin() * 20_000.0) as i32 + (i * 7919 % 64)) << 8).collect();
        let path = std::env::temp_dir().join(format!("mitschnitt-wasted-{}.flac", std::process::id()));
        write(&path, &Audio { rate: 44_100, channels: 2, bits: 24, samples: samples.clone() }, &Tags::default()).unwrap();
        let wide = std::fs::metadata(&path).unwrap().len();
        write(&path, &Audio { rate: 44_100, channels: 2, bits: 16, samples: samples.iter().map(|s| s >> 8).collect() }, &Tags::default()).unwrap();
        let narrow = std::fs::metadata(&path).unwrap().len();
        std::fs::remove_file(&path).unwrap();
        assert!(wide <= narrow + narrow / 50, "24-bit {wide} bytes, 16-bit {narrow} bytes");
    }
}

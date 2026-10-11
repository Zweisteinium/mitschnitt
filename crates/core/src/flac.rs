//! FLAC files: reading into [`Audio`], and writing a recording as a verified 24-bit FLAC.

use std::path::Path;

use flacenc::component::{BitRepr, Stream};
use flacenc::config::Encoder;
use flacenc::error::{Verified, Verify};
use flacenc::source::{Context, Fill, FrameBuf};

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
}

#[derive(Debug, Clone)]
pub struct Cover {
    pub mime: &'static str,
    pub data: Vec<u8>,
}

pub fn read(path: &Path) -> Result<Audio, claxon::Error> {
    let mut reader = claxon::FlacReader::open(path)?;
    let info = reader.streaminfo();
    let mut samples = Vec::with_capacity(info.samples.unwrap_or(0) as usize * info.channels as usize);
    for s in reader.samples() {
        samples.push(s?);
    }
    Ok(Audio { rate: info.sample_rate, channels: info.channels as usize, bits: info.bits_per_sample, samples })
}

/// Encode `audio` losslessly to `path` (written to `<path>.part` first), tag it, then decode the file
/// again and compare every sample - a file only gets its final name once it is proven identical.
pub fn write(path: &Path, audio: &Audio, tags: &Tags) -> Result<(), String> {
    let stream = encode(audio)?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|e| format!("serialize: {e:?}"))?;

    let part = path.with_extension("flac.part");
    std::fs::write(&part, sink.as_slice()).map_err(|e| format!("write {}: {e}", part.display()))?;
    let result = (|| {
        let mut tag = metaflac::Tag::read_from_path(&part).map_err(|e| format!("tags: {e}"))?;
        for (key, value) in [("TITLE", &tags.title), ("ARTIST", &tags.artist), ("ALBUM", &tags.album), ("ALBUMARTIST", &tags.album_artist)] {
            if !value.is_empty() {
                tag.set_vorbis(key, vec![value.as_str()]);
            }
        }
        if tags.track_number > 0 {
            tag.set_vorbis("TRACKNUMBER", vec![tags.track_number.to_string()]);
        }
        if let Some(cover) = &tags.cover {
            tag.add_picture(cover.mime, metaflac::block::PictureType::CoverFront, cover.data.clone());
        }
        tag.set_vorbis("ENCODER", vec!["Mitschnitt"]);
        tag.save().map_err(|e| format!("tags: {e}"))?;

        let back = read(&part).map_err(|e| format!("verify: {e}"))?;
        if back.samples != audio.samples || back.rate != audio.rate || back.channels != audio.channels {
            return Err("verify: decoded file differs from the recording".to_owned());
        }
        std::fs::rename(&part, path).map_err(|e| format!("rename: {e}"))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

/// Encode frame by frame. flacenc's Rice cost estimate overflows on loud 24-bit audio, so it can pick a fixed
/// predictor that costs several times the raw samples: every frame is checked and re-encoded with fewer
/// tools (no fixed predictor, then verbatim) until it is no larger than the raw samples.
fn encode(audio: &Audio) -> Result<Stream, String> {
    let err = |e: &dyn std::fmt::Debug| format!("encode: {e:?}");
    let configs = [(true, true), (false, true), (false, false)].map(|(fixed, lpc)| {
        let mut config = Encoder::default();
        config.subframe_coding.use_fixed = fixed;
        config.subframe_coding.use_lpc = lpc;
        config.into_verified().map_err(|(_, e)| format!("encoder config: {e:?}"))
    });
    let configs: Vec<Verified<Encoder>> = configs.into_iter().collect::<Result<_, _>>()?;
    let (channels, bits, block) = (audio.channels, audio.bits as usize, configs[0].block_size);

    let mut stream = Stream::new(audio.rate as usize, channels, bits).map_err(|e| err(&e))?;
    let mut buf = (FrameBuf::with_size(channels, block).map_err(|e| err(&e))?, Context::new(bits, channels));
    for chunk in audio.samples.chunks(block * channels) {
        buf.fill_interleaved(chunk).map_err(|e| err(&e))?;
        let number = buf.1.current_frame_number().ok_or("encode: frame number")?;
        // Raw samples plus room for the side channel's extra bit and the frame and subframe headers.
        let limit = chunk.len() * (bits + 1) + 256;
        let mut frame = None;
        for config in &configs {
            let candidate = flacenc::encode_fixed_size_frame(config, &buf.0, number, stream.stream_info()).map_err(|e| err(&e))?;
            let fits = candidate.count_bits() <= limit;
            frame = Some(candidate);
            if fits {
                break;
            }
        }
        stream.add_frame(frame.ok_or("encode: no frame")?);
    }
    // A shorter last frame still belongs to a fixed-blocksize stream (min = max, as libFLAC writes it).
    stream.stream_info_mut().set_block_sizes(block, block).map_err(|e| err(&e))?;
    stream.stream_info_mut().set_md5_digest(&buf.1.md5_digest());
    stream.stream_info_mut().set_total_samples(audio.frames());
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_is_bit_identical_and_tagged() {
        let samples: Vec<i32> = (0..44_100 * 2).map(|i| ((i * 7919) % 16_000_000) - 8_000_000).collect();
        let audio = Audio { rate: 44_100, channels: 2, bits: 24, samples };
        let path = std::env::temp_dir().join(format!("mitschnitt-test-{}.flac", std::process::id()));
        let tags = Tags { title: "Title".into(), artist: "Artist".into(), track_number: 7, cover: Some(Cover { mime: "image/jpeg", data: vec![0xFF, 0xD8, 1, 2] }), ..Tags::default() };
        write(&path, &audio, &tags).unwrap();
        let back = read(&path).unwrap();
        assert_eq!(back.samples, audio.samples);
        let tag = metaflac::Tag::read_from_path(&path).unwrap();
        assert_eq!(tag.get_vorbis("ARTIST").unwrap().next(), Some("Artist"));
        assert_eq!(tag.get_vorbis("TRACKNUMBER").unwrap().next(), Some("7"));
        assert_eq!(tag.pictures().count(), 1);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn loud_24bit_audio_stays_compact_and_seekable() {
        // Loud, slightly lowpassed noise: flacenc alone writes it at 2.6 times the raw size.
        let mut x: u32 = 1;
        let mut noise = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as f64 / (1u32 << 23) as f64 - 1.0
        };
        let mut v = 0.0;
        let samples: Vec<i32> = (0..44_100 * 2)
            .map(|i| {
                v = 0.9 * v + 0.8 * noise() + 0.06 * (i as f64 * 0.015).sin();
                (v.clamp(-1.0, 1.0) * 8_388_607.0) as i32
            })
            .collect();
        let audio = Audio { rate: 44_100, channels: 2, bits: 24, samples };
        let path = std::env::temp_dir().join(format!("mitschnitt-loud-{}.flac", std::process::id()));
        write(&path, &audio, &Tags::default()).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let raw = audio.samples.len() * 3;
        assert!(bytes.len() <= raw + raw / 50, "{} bytes for {raw} bytes of PCM", bytes.len());
        // STREAMINFO: equal min and max block size mark a fixed-blocksize stream (frame numbers, seekable).
        assert_eq!(bytes[8..10], bytes[10..12]);
    }
}

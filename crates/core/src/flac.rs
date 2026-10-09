//! FLAC files: reading into [`Audio`], and writing a recording as a verified 24-bit FLAC.

use std::path::Path;

use flacenc::component::BitRepr;
use flacenc::error::Verify;
use flacenc::source::{Fill, Source};

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
    let config = flacenc::config::Encoder::default().into_verified().map_err(|(_, e)| format!("encoder config: {e:?}"))?;
    let source = SliceSource { audio, head: 0 };
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size).map_err(|e| format!("encode: {e:?}"))?;
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

/// Zero-copy flacenc source over an [`Audio`] buffer.
struct SliceSource<'a> {
    audio: &'a Audio,
    head: usize,
}

impl Source for SliceSource<'_> {
    fn channels(&self) -> usize {
        self.audio.channels
    }

    fn bits_per_sample(&self) -> usize {
        self.audio.bits as usize
    }

    fn sample_rate(&self) -> usize {
        self.audio.rate as usize
    }

    fn read_samples<F: Fill>(&mut self, block_size: usize, dest: &mut F) -> Result<usize, flacenc::error::SourceError> {
        let ch = self.audio.channels;
        let begin = (self.head * ch).min(self.audio.samples.len());
        let end = ((self.head + block_size) * ch).min(self.audio.samples.len());
        dest.fill_interleaved(&self.audio.samples[begin..end])?;
        let frames = (end - begin) / ch;
        self.head += frames;
        Ok(frames)
    }

    fn len_hint(&self) -> Option<usize> {
        Some(self.audio.frames())
    }
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
}

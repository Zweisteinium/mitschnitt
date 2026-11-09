//! Opus in Ogg (RFC 7845): libopus at 256 kbit/s, after resampling to the 48 kHz every Opus stream runs at.

use std::io::BufWriter;
use std::path::Path;

use base64::Engine as _;
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

use crate::audio::Audio;
use crate::flac::Tags;
use crate::output::via_part;

const RATE: u32 = 48_000;
/// 20 ms, the frame size libopus is tuned for.
const FRAME: usize = 960;
const BITRATE: i32 = 256_000;
const SERIAL: u32 = 0x4d69_7473;

pub fn write(path: &Path, audio: &Audio, tags: &Tags) -> Result<(), String> {
    if audio.channels != 2 {
        return Err(format!("opus: {} channels (stereo only)", audio.channels));
    }
    let err = |e: &dyn std::fmt::Display| format!("opus: {e}");
    let scale = 1.0 / (1u64 << (audio.bits - 1)) as f32;
    let pcm: Vec<f32> = audio.samples.iter().map(|s| *s as f32 * scale).collect();
    let mut pcm = resample(&pcm, audio.rate)?;
    // The song's own length at 48 kHz: the resampler may hand out a frame more (48 001 for a second, measured).
    let frames = (audio.frames() as u64 * RATE as u64).div_ceil(audio.rate as u64) as usize;
    pcm.resize(frames * 2, 0.0);

    let mut encoder = opus::Encoder::new(RATE, opus::Channels::Stereo, opus::Application::Audio).map_err(|e| err(&e))?;
    encoder.set_bitrate(opus::Bitrate::Bits(BITRATE)).map_err(|e| err(&e))?;
    encoder.set_signal(opus::Signal::Music).map_err(|e| err(&e))?;
    let pre_skip = encoder.get_lookahead().map_err(|e| err(&e))? as u64;

    via_part(path, "opus.part", |part| {
        let file = std::fs::File::create(part).map_err(|e| format!("write {}: {e}", part.display()))?;
        let mut ogg = PacketWriter::new(BufWriter::new(file));
        let mut head = b"OpusHead".to_vec();
        head.extend([1, 2]);
        head.extend((pre_skip as u16).to_le_bytes());
        head.extend(audio.rate.to_le_bytes());
        head.extend([0, 0, 0]); // no output gain, channel mapping family 0
        ogg.write_packet(head, SERIAL, PacketWriteEndInfo::EndPage, 0).map_err(|e| err(&e))?;
        ogg.write_packet(comments(tags), SERIAL, PacketWriteEndInfo::EndPage, 0).map_err(|e| err(&e))?;

        // The decoder drops the encoder's lookahead at the start: the song's last `pre_skip` samples need packets too.
        let packets = (frames + pre_skip as usize).div_ceil(FRAME);
        let mut frame = vec![0.0f32; FRAME * 2];
        for i in 0..packets {
            let chunk = &pcm[(i * FRAME * 2).min(pcm.len())..((i + 1) * FRAME * 2).min(pcm.len())];
            frame[..chunk.len()].copy_from_slice(chunk);
            frame[chunk.len()..].fill(0.0);
            let packet = encoder.encode_vec_float(&frame, 4000).map_err(|e| err(&e))?;
            // The last page's position ends the song where it ends: players cut the padding of the last frame.
            let last = i + 1 == packets;
            let position = pre_skip + if last { frames as u64 } else { ((i + 1) * FRAME) as u64 };
            let end = if last { PacketWriteEndInfo::EndStream } else { PacketWriteEndInfo::NormalPacket };
            ogg.write_packet(packet, SERIAL, end, position).map_err(|e| err(&e))?;
        }
        use std::io::Write as _;
        ogg.into_inner().flush().map_err(|e| err(&e))
    })
}

fn resample(pcm: &[f32], rate: u32) -> Result<Vec<f32>, String> {
    if rate == RATE {
        return Ok(pcm.to_vec());
    }
    let err = |e: &dyn std::fmt::Display| format!("resample: {e}");
    let frames = pcm.len() / 2;
    let mut resampler = Fft::<f32>::new(rate as usize, RATE as usize, 1024, 2, FixedSync::Input).map_err(|e| err(&e))?;
    let input = InterleavedSlice::new(pcm, 2, frames).map_err(|e| err(&e))?;
    Ok(resampler.process_all(&input, frames, None).map_err(|e| err(&e))?.take_data())
}

/// Vorbis comments, the cover as a FLAC picture block (`METADATA_BLOCK_PICTURE`, as opusenc writes it).
fn comments(tags: &Tags) -> Vec<u8> {
    let mut fields: Vec<String> = tags.vorbis().into_iter().map(|(k, v)| format!("{k}={v}")).collect();
    // An image it can't measure stays out, as in the FLAC files.
    if let Some(picture) = tags.cover.as_ref().and_then(|c| flac_codec::metadata::Picture::new(flac_codec::metadata::PictureType::FrontCover, "", c.data.clone()).ok()) {
        fields.push(format!("METADATA_BLOCK_PICTURE={}", base64::engine::general_purpose::STANDARD.encode(picture_block(&picture))));
    }
    let vendor = opus::version();
    let mut out = b"OpusTags".to_vec();
    out.extend((vendor.len() as u32).to_le_bytes());
    out.extend(vendor.as_bytes());
    out.extend((fields.len() as u32).to_le_bytes());
    for field in fields {
        out.extend((field.len() as u32).to_le_bytes());
        out.extend(field.as_bytes());
    }
    out
}

fn picture_block(p: &flac_codec::metadata::Picture) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(3u32.to_be_bytes()); // front cover
    b.extend((p.media_type.len() as u32).to_be_bytes());
    b.extend(p.media_type.as_bytes());
    b.extend(0u32.to_be_bytes()); // no description
    for v in [p.width, p.height, p.color_depth, p.colors_used.map_or(0, |n| n.get())] {
        b.extend(v.to_be_bytes());
    }
    b.extend((p.data.len() as u32).to_be_bytes());
    b.extend(&p.data);
    b
}

/// The Vorbis comments of a file written by [`write`], each `KEY=value`.
pub fn comments_of(path: &Path) -> Option<Vec<String>> {
    let mut reader = ogg::PacketReader::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
    reader.read_packet().ok()??; // OpusHead
    let tags = reader.read_packet().ok()??.data;
    let u32_at = |i: usize| tags.get(i..i + 4).map(|x| u32::from_le_bytes(x.try_into().unwrap()) as usize);
    let mut at = 8 + 4 + u32_at(8)?;
    let count = u32_at(at)?;
    at += 4;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let len = u32_at(at)?;
        fields.push(String::from_utf8_lossy(tags.get(at + 4..at + 4 + len)?).into_owned());
        at += 4 + len;
    }
    Some(fields)
}

/// A `METADATA_BLOCK_PICTURE` value: the image's mime type and data.
pub fn picture(value: &str) -> Option<(String, Vec<u8>)> {
    let block = base64::engine::general_purpose::STANDARD.decode(value).ok()?;
    let be = |i: usize| block.get(i..i + 4).map(|x| u32::from_be_bytes(x.try_into().unwrap()) as usize);
    let mime_len = be(4)?;
    let mime = String::from_utf8(block.get(8..8 + mime_len)?.to_vec()).ok()?;
    let data_at = 8 + mime_len + 4 + be(8 + mime_len)? + 16;
    Some((mime, block.get(data_at + 4..data_at + 4 + be(data_at)?)?.to_vec()))
}

pub fn cover(path: &Path) -> Option<(String, Vec<u8>)> {
    comments_of(path)?.iter().find_map(|f| f.strip_prefix("METADATA_BLOCK_PICTURE=")).and_then(picture)
}

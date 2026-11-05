//! Apple Lossless in an .m4a, encoded in Rust: no codec to install. The tags go in afterwards (`output::m4a_tags`).
//!
//! The file is the plainest MP4 that iTunes and ffmpeg write for ALAC: `ftyp`, the packets in `mdat`, then `moov`
//! with one track whose `stsd` carries the encoder's magic cookie. `moov` comes last, so adding tags to it never moves
//! the packets that its chunk offsets point to.

use std::path::Path;

use alac_encoder::{AlacEncoder, FormatDescription, PcmFormat};

use crate::audio::Audio;

/// Apple's default, about 93 ms at 44.1 kHz.
const FRAMES_PER_PACKET: usize = 4096;

/// Packed 24-bit little-endian input, which the encoder takes as it is.
struct S24;

impl PcmFormat for S24 {
    fn bits() -> u32 {
        24
    }

    fn bytes() -> u32 {
        3
    }

    fn flags() -> u32 {
        0
    }
}

/// `audio` at 16 or 24 bit, every sample kept.
pub fn write(path: &Path, audio: &Audio) -> Result<(), String> {
    let file = encode(audio)?;
    std::fs::write(path, file).map_err(|e| format!("{}: {e}", path.display()))
}

fn encode(audio: &Audio) -> Result<Vec<u8>, String> {
    if !matches!(audio.bits, 16 | 24) || !(1..=8).contains(&audio.channels) {
        return Err(format!("ALAC: {} bit, {} channels", audio.bits, audio.channels));
    }
    let (rate, channels, bytes) = (audio.rate, audio.channels as u32, audio.bits as usize / 8);
    let output = FormatDescription::alac(rate as f64, FRAMES_PER_PACKET as u32, channels, audio.bits);
    let input = if audio.bits == 16 { FormatDescription::pcm::<i16>(rate as f64, channels) } else { FormatDescription::pcm::<S24>(rate as f64, channels) };
    let mut encoder = AlacEncoder::new(&output);
    let mut packet = vec![0u8; output.max_packet_size() + 8];
    let mut pcm = Vec::with_capacity(FRAMES_PER_PACKET * audio.channels * bytes);
    let (mut data, mut sizes) = (Vec::new(), Vec::new());
    for chunk in audio.samples.chunks(FRAMES_PER_PACKET * audio.channels) {
        pcm.clear();
        for sample in chunk {
            pcm.extend_from_slice(&sample.to_le_bytes()[..bytes]);
        }
        let size = encoder.encode(&input, &pcm, &mut packet);
        data.extend_from_slice(&packet[..size]);
        sizes.push(size as u32);
    }

    let frames = u32::try_from(audio.frames()).map_err(|_| "ALAC: too long for an .m4a")?;
    let ftyp = mp4_box(b"ftyp", &[b"M4A ".as_slice(), &0u32.to_be_bytes(), b"M4A mp42isom"].concat());
    let start = ftyp.len() + 8;
    let mut offsets = Vec::with_capacity(sizes.len());
    let mut at = start;
    for size in &sizes {
        offsets.push(u32::try_from(at).map_err(|_| "ALAC: over 4 GB")?);
        at += *size as usize;
    }
    let mdat_size = u32::try_from(8 + data.len()).map_err(|_| "ALAC: over 4 GB")?;
    let moov = moov(rate, channels as u16, audio.bits as u16, frames, &encoder.magic_cookie(), &sizes, &offsets);

    let mut file = ftyp;
    file.extend_from_slice(&mdat_size.to_be_bytes());
    file.extend_from_slice(b"mdat");
    file.extend_from_slice(&data);
    file.extend_from_slice(&moov);
    Ok(file)
}

fn moov(rate: u32, channels: u16, bits: u16, frames: u32, cookie: &[u8], sizes: &[u32], offsets: &[u32]) -> Vec<u8> {
    const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];
    let matrix: Vec<u8> = MATRIX.iter().flat_map(|v| v.to_be_bytes()).collect();
    let u16s = |v: &[u16]| v.iter().flat_map(|v| v.to_be_bytes()).collect::<Vec<u8>>();
    let u32s = |v: &[u32]| v.iter().flat_map(|v| v.to_be_bytes()).collect::<Vec<u8>>();

    // Timescales are the sample rate, so durations count frames exactly.
    let mvhd = full_box(b"mvhd", 0, &[u32s(&[0, 0, rate, frames, 0x0001_0000]), u16s(&[0x0100]), vec![0; 10], matrix.clone(), vec![0; 24], u32s(&[2])].concat());
    let tkhd = full_box(b"tkhd", 7, &[u32s(&[0, 0, 1, 0, frames]), vec![0; 8], u16s(&[0, 0, 0x0100, 0]), matrix, u32s(&[0, 0])].concat());
    // "und": three 5-bit letters, each minus 0x60.
    let mdhd = full_box(b"mdhd", 0, &[u32s(&[0, 0, rate, frames]), u16s(&[0x55C4, 0])].concat());
    let hdlr = full_box(b"hdlr", 0, &[u32s(&[0]), b"soun".to_vec(), vec![0; 12], b"SoundHandler\0".to_vec()].concat());
    let smhd = full_box(b"smhd", 0, &[0; 4]);
    let dinf = mp4_box(b"dinf", &full_box(b"dref", 0, &[u32s(&[1]), full_box(b"url ", 1, &[])].concat()));

    // The 16.16 rate field only holds up to 65535 Hz; the cookie carries the real one.
    let rate_field = if rate <= 0xFFFF { rate << 16 } else { 0 };
    let entry = mp4_box(b"alac", &[vec![0; 6], u16s(&[1, 0, 0]), u32s(&[0]), u16s(&[channels, bits, 0, 0]), u32s(&[rate_field]), full_box(b"alac", 0, cookie)].concat());
    let stsd = full_box(b"stsd", 0, &[u32s(&[1]), entry].concat());
    let (full, last) = (frames / FRAMES_PER_PACKET as u32, frames % FRAMES_PER_PACKET as u32);
    let mut stts_entries = Vec::new();
    if full > 0 {
        stts_entries.extend([full, FRAMES_PER_PACKET as u32]);
    }
    if last > 0 {
        stts_entries.extend([1, last]);
    }
    let stts = full_box(b"stts", 0, &[u32s(&[stts_entries.len() as u32 / 2]), u32s(&stts_entries)].concat());
    // One packet per chunk.
    let stsc = full_box(b"stsc", 0, &u32s(&[1, 1, 1, 1]));
    let stsz = full_box(b"stsz", 0, &[u32s(&[0, sizes.len() as u32]), u32s(sizes)].concat());
    let stco = full_box(b"stco", 0, &[u32s(&[offsets.len() as u32]), u32s(offsets)].concat());
    let stbl = mp4_box(b"stbl", &[stsd, stts, stsc, stsz, stco].concat());

    let minf = mp4_box(b"minf", &[smhd, dinf, stbl].concat());
    let mdia = mp4_box(b"mdia", &[mdhd, hdlr, minf].concat());
    let trak = mp4_box(b"trak", &[tkhd, mdia].concat());
    mp4_box(b"moov", &[mvhd, trak].concat())
}

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(8 + body.len());
    b.extend_from_slice(&(8 + body.len() as u32).to_be_bytes());
    b.extend_from_slice(kind);
    b.extend_from_slice(body);
    b
}

/// A box with a version (always 0 here) and 24 bits of flags.
fn full_box(kind: &[u8; 4], flags: u32, body: &[u8]) -> Vec<u8> {
    mp4_box(kind, &[&flags.to_be_bytes(), body].concat())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Every sample of an .m4a, decoded by Symphonia (a decoder that isn't this encoder), full scale in 32 bits.
    /// Rate, channels, the codec's setup data and the samples.
    pub(crate) fn decode(path: &Path) -> (u32, usize, Box<[u8]>, Vec<i32>) {
        use symphonia::core::codecs::audio::AudioDecoderOptions;
        use symphonia::core::formats::probe::Hint;
        use symphonia::core::formats::{FormatOptions, TrackType};
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;

        let source = MediaSourceStream::new(Box::new(std::fs::File::open(path).unwrap()), Default::default());
        let mut format = symphonia::default::get_probe().probe(Hint::new().with_extension("m4a"), source, FormatOptions::default(), MetadataOptions::default()).unwrap();
        let track = format.default_track(TrackType::Audio).unwrap();
        let (id, params) = (track.id, track.codec_params.as_ref().unwrap().audio().unwrap().clone());
        let mut decoder = symphonia::default::get_codecs().make_audio_decoder(&params, &AudioDecoderOptions::default()).unwrap();
        let (mut samples, mut packet_samples) = (Vec::new(), Vec::<i32>::new());
        while let Some(packet) = format.next_packet().unwrap() {
            if packet.track_id != id {
                continue;
            }
            decoder.decode(&packet).unwrap().copy_to_vec_interleaved(&mut packet_samples);
            samples.extend_from_slice(&packet_samples);
        }
        (params.sample_rate.unwrap(), params.channels.unwrap().count(), params.extra_data.unwrap_or_default(), samples)
    }

    /// Rate, the width the file's cookie declares, channels, and the samples at that width.
    fn decode_alac(path: &Path) -> (u32, u32, usize, Vec<i32>) {
        let (rate, channels, cookie, samples) = decode(path);
        let bits = cookie[5] as u32;
        (rate, bits, channels, samples.into_iter().map(|s| s >> (32 - bits)).collect())
    }

    /// Full scale and silence, steps and noise, and a length that isn't a multiple of the packet size.
    fn signal(bits: u32, channels: usize) -> Audio {
        let max = (1i32 << (bits - 1)) - 1;
        let mut noise = 0x2545_F491u32;
        let samples = (0..(3 * FRAMES_PER_PACKET + 1234) * channels)
            .map(|i| match i / channels / 3000 {
                0 => 0,
                1 => {
                    noise ^= noise << 13;
                    noise ^= noise >> 17;
                    noise ^= noise << 5;
                    (noise as i32) >> (33 - bits)
                }
                2 => if i % 2 == 0 { max } else { -max - 1 },
                _ => ((i as f64 * 0.01).sin() * max as f64 * 0.8) as i32,
            })
            .collect();
        Audio { rate: 44_100, channels, bits, samples }
    }

    #[test]
    fn every_sample_comes_back_at_16_and_24_bit() {
        let dir = std::env::temp_dir().join(format!("mitschnitt-alac-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (bits, channels) in [(16, 2), (24, 2), (24, 1), (16, 1)] {
            let audio = signal(bits, channels);
            let path = dir.join(format!("{bits}-{channels}.m4a"));
            write(&path, &audio).unwrap();
            let (rate, decoded_bits, decoded_channels, samples) = decode_alac(&path);
            assert_eq!((rate, decoded_bits, decoded_channels), (44_100, bits, channels), "{bits} bit, {channels} channels");
            assert_eq!(samples.len(), audio.samples.len(), "{bits} bit, {channels} channels");
            if let Some(i) = (0..samples.len()).find(|&i| samples[i] != audio.samples[i]) {
                panic!("{bits} bit, {channels} channels: sample {i} went in as {} and came back as {}", audio.samples[i], samples[i]);
            }
            assert!(std::fs::metadata(&path).unwrap().len() < (audio.samples.len() * bits as usize / 8) as u64, "compressed");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A second decoder, where it is installed: ffmpeg's own ALAC and MP4 code.
    #[test]
    fn ffmpeg_reads_every_sample_too() {
        use std::process::Command;
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            return eprintln!("no ffmpeg, skipped");
        }
        let dir = std::env::temp_dir().join(format!("mitschnitt-alac-ffmpeg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for bits in [16, 24] {
            let audio = signal(bits, 2);
            let path = dir.join(format!("{bits}.m4a"));
            write(&path, &audio).unwrap();
            let out = Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(&path).args(["-f", "s32le", "-"]).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let samples: Vec<i32> = out.stdout.chunks_exact(4).map(|b| i32::from_le_bytes(b.try_into().unwrap()) >> (32 - bits)).collect();
            assert!(samples == audio.samples, "{bits} bit: {} samples back for {}", samples.len(), audio.samples.len());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_song_shorter_than_one_packet_is_whole() {
        let audio = Audio { rate: 48_000, channels: 2, bits: 24, samples: (0..2000).map(|i| i * 997 % 8_000_000 - 4_000_000).collect() };
        let path = std::env::temp_dir().join(format!("mitschnitt-alac-short-{}.m4a", std::process::id()));
        write(&path, &audio).unwrap();
        let (rate, _, _, samples) = decode_alac(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!((rate, samples), (48_000, audio.samples));
    }
}

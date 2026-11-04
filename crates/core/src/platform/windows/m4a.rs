//! .m4a by Windows' own Media Foundation encoders: AAC at 192 kbit/s (its highest rate) and ALAC, lossless at 16 or
//! 24 bit. No codec ships with Mitschnitt for them.

use std::path::Path;

use windows::Win32::Media::MediaFoundation::*;
use windows::core::HSTRING;

use crate::audio::Audio;

pub const SUPPORTED: bool = true;

const AAC_BYTES_PER_SECOND: u32 = 24_000;

/// `audio` at 16 or 24 bit; AAC takes 16-bit input only, so 24-bit audio is rounded for it.
pub fn write(path: &Path, audio: &Audio, alac: bool) -> Result<(), String> {
    super::devices::com_init();
    unsafe {
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| format!("m4a: {e}"))?;
        let result = encode(path, audio, alac);
        let _ = MFShutdown();
        result.map_err(|e| format!("m4a ({}): {e}", if alac { "ALAC" } else { "AAC" }))
    }
}

unsafe fn encode(path: &Path, audio: &Audio, alac: bool) -> windows::core::Result<()> {
    let bits = if alac { audio.bits } else { 16 };
    let (bytes, channels, rate) = (bits / 8, audio.channels as u32, audio.rate);
    let audio_type = |subtype: &windows::core::GUID| -> windows::core::Result<IMFMediaType> {
        let t = unsafe { MFCreateMediaType()? };
        unsafe {
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
            t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
            t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
            t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, bits)?;
        }
        Ok(t)
    };
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1)?;
        let attributes = attributes.ok_or_else(windows::core::Error::empty)?;
        // The file ends in ".part" while it is written: the container can't come from the extension.
        attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attributes)?;

        let output = audio_type(if alac { &MFAudioFormat_ALAC } else { &MFAudioFormat_AAC })?;
        if !alac {
            output.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SECOND)?;
        }
        let stream = writer.AddStream(&output)?;
        let input = audio_type(&MFAudioFormat_PCM)?;
        input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, channels * bytes)?;
        input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * channels * bytes)?;
        writer.SetInputMediaType(stream, &input, None)?;
        writer.BeginWriting()?;

        let shift = audio.bits - bits;
        let half = if shift > 0 { 1 << (shift - 1) } else { 0 };
        let per_sample = rate as usize; // one second
        for (i, chunk) in audio.samples.chunks(per_sample * channels as usize).enumerate() {
            let data: Vec<u8> = chunk.iter().flat_map(|s| (((s + half) >> shift).clamp(-(1 << (bits - 1)), (1 << (bits - 1)) - 1)).to_le_bytes().into_iter().take(bytes as usize)).collect();
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut target = std::ptr::null_mut();
            buffer.Lock(&mut target, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            let frames = (chunk.len() / channels as usize) as i64;
            sample.SetSampleTime((i * per_sample) as i64 * 10_000_000 / rate as i64)?;
            sample.SetSampleDuration(frames * 10_000_000 / rate as i64)?;
            writer.WriteSample(stream, &sample)?;
        }
        writer.Finalize()
    }
}

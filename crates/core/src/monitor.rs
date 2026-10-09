//! Built-in monitoring: plays the captured audio on the default output device, so the app itself can
//! render into a silent virtual device. Replaces Windows' "Listen to this device" and owns the loudness:
//! the player runs at the recording gain, the monitor undoes it times the user's volume.

use std::collections::VecDeque;
use std::ptr::addr_of;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient, IAudioRenderClient, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0, eConsole, eRender,
};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::devices::{com_init, enumerator};

/// Keep latency bounded: beyond this much queued audio the oldest is dropped.
const MAX_QUEUE_MS: usize = 250;
const TARGET_QUEUE_MS: usize = 60;

pub struct Monitor {
    queue: Arc<Mutex<VecDeque<f32>>>,
    volume: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    channels: usize,
    max_samples: usize,
    target_samples: usize,
}

impl Monitor {
    /// `gain` is the recording gain the player applies; it is undone here.
    pub fn start(rate: u32, channels: u16, gain: f32, volume: f32) -> Monitor {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let volume = Arc::new(AtomicU32::new(volume.to_bits()));
        let stop = Arc::new(AtomicBool::new(false));
        let (q, v, s) = (queue.clone(), volume.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("monitor".into())
            .spawn(move || {
                com_init();
                // Reopen on errors (e.g. the headphones disconnect and the default device changes).
                while !s.load(Ordering::Relaxed) {
                    let _ = Output::open(rate, channels).and_then(|out| out.run(&q, &v, 1.0 / gain, &s));
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
            .ok();
        let per_ms = rate as usize * channels as usize / 1000;
        Monitor {
            queue,
            volume,
            stop,
            thread,
            channels: channels as usize,
            max_samples: MAX_QUEUE_MS * per_ms,
            target_samples: TARGET_QUEUE_MS * per_ms,
        }
    }

    /// Queue one captured packet (silent packets are queued as silence to keep timing).
    pub fn feed(&self, data: &[f32], frames: usize) {
        let Ok(mut q) = self.queue.lock() else { return };
        if data.len() >= frames * self.channels {
            q.extend(data.iter().copied());
        } else {
            q.extend(std::iter::repeat_n(0.0, frames * self.channels));
        }
        if q.len() > self.max_samples {
            let excess = q.len() - self.target_samples;
            q.drain(..excess - excess % self.channels);
        }
    }

    pub fn set_volume(&self, volume: f32) {
        self.volume.store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn volume_handle(&self) -> Arc<AtomicU32> {
        self.volume.clone()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Output {
    client: IAudioClient,
    render: IAudioRenderClient,
    event: HANDLE,
    buffer_frames: u32,
    channels: usize,
}

impl Output {
    fn open(rate: u32, channels: u16) -> windows::core::Result<Output> {
        let block = channels * 4;
        let format = WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: 0xFFFE,
                nChannels: channels,
                nSamplesPerSec: rate,
                nAvgBytesPerSec: rate * block as u32,
                nBlockAlign: block,
                wBitsPerSample: 32,
                cbSize: 22,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
            dwChannelMask: if channels == 1 { 0x4 } else { 0x3 },
            SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        };
        // SAFETY: COM calls on interfaces we own; Windows resamples to the device rate (monitoring only).
        unsafe {
            let device = enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                400_000, // 40 ms
                0,
                addr_of!(format).cast::<WAVEFORMATEX>(),
                None,
            )?;
            let event = CreateEventW(None, false, false, None)?;
            client.SetEventHandle(event)?;
            let render: IAudioRenderClient = client.GetService()?;
            let buffer_frames = client.GetBufferSize()?;
            client.Start()?;
            Ok(Output { client, render, event, buffer_frames, channels: channels as usize })
        }
    }

    fn run(&self, queue: &Mutex<VecDeque<f32>>, volume: &AtomicU32, restore: f32, stop: &AtomicBool) -> windows::core::Result<()> {
        // SAFETY: the render buffer is valid for `free * channels` floats until ReleaseBuffer.
        unsafe {
            while !stop.load(Ordering::Relaxed) {
                WaitForSingleObject(self.event, 100);
                let free = self.buffer_frames - self.client.GetCurrentPadding()?;
                if free == 0 {
                    continue;
                }
                let out = std::slice::from_raw_parts_mut(self.render.GetBuffer(free)?.cast::<f32>(), free as usize * self.channels);
                let gain = restore * f32::from_bits(volume.load(Ordering::Relaxed));
                {
                    let mut q = queue.lock().map_err(|_| windows::core::Error::from(E_FAIL))?;
                    for sample in out.iter_mut() {
                        *sample = q.pop_front().map_or(0.0, |s| (s * gain).clamp(-1.0, 1.0));
                    }
                }
                self.render.ReleaseBuffer(free, 0)?;
            }
        }
        Ok(())
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        // SAFETY: stopping our own client and closing our own event.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

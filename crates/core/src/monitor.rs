//! Built-in monitoring: plays the captured audio on the default output device, so the app itself can
//! render into a silent virtual device. Replaces Windows' "Listen to this device" while listening: the
//! player runs at the recording gain and the monitor undoes it.
//!
//! The monitor's volume *is* its Windows mixer entry (session volume): the app's slider sets it, and changes
//! made in the mixer flow back. Windows remembers it per app, so there is exactly one value in two places.
//!
//! The monitor stays on the output device it started on ("home"). When Windows switches the default output
//! - typically headphones taken off or disconnected - it mutes instead of suddenly playing on the speakers,
//! until the home device is back or the user unmutes (which makes the new device home).

use std::collections::VecDeque;
use std::ptr::addr_of;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient, IAudioRenderClient, ISimpleAudioVolume, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0, eConsole, eRender,
};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::devices::{com_init, enumerator};

/// Keep latency bounded: beyond this much queued audio the oldest is dropped.
const MAX_QUEUE_MS: usize = 250;
const TARGET_QUEUE_MS: usize = 60;

/// Shared between the monitor thread and its owner.
#[derive(Default)]
pub struct MonitorState {
    /// Muted because the default output device changed.
    pub muted: AtomicBool,
    /// Accept the current default device as home and unmute.
    rehome: AtomicBool,
}

impl MonitorState {
    pub fn unmute(&self) {
        self.rehome.store(true, Ordering::Relaxed);
    }
}

pub struct Monitor {
    queue: Arc<Mutex<VecDeque<f32>>>,
    /// The session volume (0..1, f32 bits): written by the owner to change it, by the monitor when the
    /// mixer changed it. NaN until the session is open.
    volume: Arc<AtomicU32>,
    state: Arc<MonitorState>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    channels: usize,
    max_samples: usize,
    target_samples: usize,
}

impl Monitor {
    /// `gain` is the recording gain the player applies; it is undone here.
    pub fn start(rate: u32, channels: u16, gain: f32) -> Monitor {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let volume = Arc::new(AtomicU32::new(f32::NAN.to_bits()));
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(MonitorState::default());
        let (q, v, s, st) = (queue.clone(), volume.clone(), stop.clone(), state.clone());
        let thread = std::thread::Builder::new()
            .name("monitor".into())
            .spawn(move || {
                com_init();
                let mut home: Option<String> = None;
                // Reopen on errors and whenever the default device changes.
                while !s.load(Ordering::Relaxed) {
                    if let Ok(out) = Output::open(rate, channels) {
                        if st.rehome.swap(false, Ordering::Relaxed) || home.is_none() {
                            home = Some(out.device.clone());
                        }
                        st.muted.store(home.as_deref() != Some(out.device.as_str()), Ordering::Relaxed);
                        let _ = out.run(&q, &v, 1.0 / gain, &s, &st);
                        continue; // left because the default device changed (or stop): reopen right away
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
            .ok();
        let per_ms = rate as usize * channels as usize / 1000;
        Monitor {
            queue,
            volume,
            state,
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


    pub fn volume_handle(&self) -> Arc<AtomicU32> {
        self.volume.clone()
    }

    pub fn state(&self) -> Arc<MonitorState> {
        self.state.clone()
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
    device: String,
    client: IAudioClient,
    render: IAudioRenderClient,
    session_volume: ISimpleAudioVolume,
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
            let id = default_id()?;
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
            let session_volume: ISimpleAudioVolume = client.GetService()?;
            let buffer_frames = client.GetBufferSize()?;
            client.Start()?;
            Ok(Output { device: id, client, render, session_volume, event, buffer_frames, channels: channels as usize })
        }
    }

    /// Render until stopped, until the default device changes, or until the user unmutes.
    fn run(&self, queue: &Mutex<VecDeque<f32>>, volume: &AtomicU32, restore: f32, stop: &AtomicBool, state: &MonitorState) -> windows::core::Result<()> {
        let mut checked = std::time::Instant::now();
        // SAFETY: the render buffer is valid for `free * channels` floats until ReleaseBuffer.
        unsafe {
            // Adopt the value Windows remembers for this app, unless the owner already asked for one.
            let mut applied = self.session_volume.GetMasterVolume()?;
            if f32::from_bits(volume.load(Ordering::Relaxed)).is_nan() {
                volume.store(applied.to_bits(), Ordering::Relaxed);
            }
            while !stop.load(Ordering::Relaxed) {
                // Sync both ways: a new value from the app wins, otherwise follow the mixer.
                let wanted = f32::from_bits(volume.load(Ordering::Relaxed));
                if !wanted.is_nan() && wanted != applied {
                    self.session_volume.SetMasterVolume(wanted, std::ptr::null())?;
                    applied = wanted;
                }
                if checked.elapsed() >= Duration::from_millis(200) {
                    checked = std::time::Instant::now();
                    if default_id().ok().as_deref() != Some(self.device.as_str()) || state.rehome.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    let mixer = self.session_volume.GetMasterVolume()?;
                    if mixer != applied {
                        volume.store(mixer.to_bits(), Ordering::Relaxed);
                        applied = mixer;
                    }
                }
                WaitForSingleObject(self.event, 100);
                let free = self.buffer_frames - self.client.GetCurrentPadding()?;
                if free == 0 {
                    continue;
                }
                let out = std::slice::from_raw_parts_mut(self.render.GetBuffer(free)?.cast::<f32>(), free as usize * self.channels);
                let muted = state.muted.load(Ordering::Relaxed);
                let gain = if muted { 0.0 } else { restore }; // the volume itself is applied by Windows
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

/// Id of the current default output device.
fn default_id() -> windows::core::Result<String> {
    // SAFETY: the id string is allocated by COM and freed by `to_string` + CoTaskMemFree.
    unsafe {
        let id = enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole)?.GetId()?;
        let s = id.to_string().unwrap_or_default();
        windows::Win32::System::Com::CoTaskMemFree(Some(id.0 as _));
        Ok(s)
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

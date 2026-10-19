//! Plays the capture on the default output while the app renders into the silent cable, undoing the recording
//! gain. Its volume is its own Windows mixer entry, synced both ways with the app's slider.
//!
//! It stays on the device it started on ("home"): when the default output changes (headphones off), it mutes
//! instead of suddenly playing on the speakers, until home is back or the user unmutes.

use std::collections::VecDeque;
use std::ptr::addr_of;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient, IAudioRenderClient, ISimpleAudioVolume, WAVEFORMATEX, eConsole, eRender,
};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use super::devices::{com_init, device_id, enumerator, wave_format};
use crate::monitor::MonitorState;

pub const SUPPORTED: bool = true;

/// Beyond this much queued audio, the oldest is dropped down to the target.
const MAX_QUEUE_MS: usize = 250;
const TARGET_QUEUE_MS: usize = 60;

pub struct Monitor {
    queue: Arc<Mutex<VecDeque<f32>>>,
    /// The session volume (0..1, f32 bits), written by both sides. NaN until the session is open.
    volume: Arc<AtomicU32>,
    state: Arc<MonitorState>,
    stop: Arc<AtomicBool>,
    channels: usize,
    max_samples: usize,
    target_samples: usize,
}

impl Monitor {
    /// `gain`: the recording gain the player applies, undone here.
    /// `audible` off: no monitor, the app plays into the silent cable.
    pub fn start(rate: u32, channels: u16, gain: f32, _app_pid: u32, audible: bool) -> Option<Monitor> {
        if !audible {
            return None;
        }
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let volume = Arc::new(AtomicU32::new(f32::NAN.to_bits()));
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(MonitorState::default());
        let (q, v, s, st) = (queue.clone(), volume.clone(), stop.clone(), state.clone());
        let _ = std::thread::Builder::new()
            .name("monitor".into())
            .spawn(move || {
                com_init();
                let mut home: Option<String> = None;
                while !s.load(Ordering::Relaxed) {
                    if home.is_some() {
                        settle(&s);
                    }
                    if let Ok(out) = Output::open(rate, channels) {
                        if st.rehome.swap(false, Ordering::Relaxed) || home.is_none() {
                            home = Some(out.device.clone());
                        }
                        st.muted.store(home.as_deref() != Some(out.device.as_str()), Ordering::Relaxed);
                        crate::log!("monitor on \"{}\", home \"{}\"", out.device, home.as_deref().unwrap_or_default());
                        let _ = out.run(&q, &v, 1.0 / gain, &s, &st);
                        continue;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            });
        let per_ms = rate as usize * channels as usize / 1000;
        Some(Monitor {
            queue,
            volume,
            state,
            stop,
            channels: channels as usize,
            max_samples: MAX_QUEUE_MS * per_ms,
            target_samples: TARGET_QUEUE_MS * per_ms,
        })
    }

    /// Silent packets are queued as silence to keep timing.
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

/// Not joined: a thread caught in the audio stack's deadlock (see [`settle`]) would hang the engine's stop.
impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Bluetooth outputs appear and vanish again while (dis)connecting. Opening one that vanished during
/// `GetService` deadlocked inside Windows' audio stack (seen in a dump after switching Bluetooth on): wait until the
/// default output stayed the same for a second.
fn settle(stop: &AtomicBool) {
    let mut last = default_id().ok();
    let mut since = std::time::Instant::now();
    while since.elapsed() < Duration::from_secs(1) && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
        let now = default_id().ok();
        if now != last {
            (last, since) = (now, std::time::Instant::now());
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
        let format = wave_format(rate, channels, 32, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
        // SAFETY: COM calls on interfaces we own; Windows resamples to the device rate (monitoring only).
        unsafe {
            let device = enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let id = device_id(&device)?;
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

    /// Until stopped, the default device changes, or the user unmutes.
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
                let gain = if state.muted.load(Ordering::Relaxed) { 0.0 } else { restore }; // Windows applies the volume
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

fn default_id() -> windows::core::Result<String> {
    // SAFETY: COM call on an interface we own.
    device_id(&unsafe { enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole) }?)
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

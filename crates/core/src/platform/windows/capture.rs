//! Per-process capture via WASAPI process loopback: 32-bit float packets of one process tree,
//! independent of other apps, the device volume and the output device.

use std::ptr::{addr_of, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{CloseHandle, E_FAIL, E_NOINTERFACE, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient,
    IAudioClient, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, IAgileObject, IAgileObject_Impl};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{Error, HRESULT, IUnknown, Interface, Ref, Result};
use windows_core::implement;

use crate::capture::{CaptureEvent, Packet};

/// A running capture thread; stops when dropped.
pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Capture the process tree of `pid` as `channels` x 32-bit float at `rate` (use the engine rate of the
    /// device the app plays on, so Windows doesn't resample a second time).
    pub fn start(pid: u32, rate: u32, channels: u16, mut sink: impl FnMut(CaptureEvent) + Send + 'static) -> std::result::Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                // SAFETY: plain COM initialisation of this thread.
                let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                match Stream::open(pid, rate, channels) {
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("could not open the capture: {e}")));
                    }
                    Ok(stream) => {
                        let _ = ready_tx.send(Ok(()));
                        if let Err(e) = stream.run(&flag, &mut sink) {
                            sink(CaptureEvent::Error(e.to_string()));
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv().map_err(|e| e.to_string())??;
        Ok(Capture { stop, thread: Some(thread) })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
    channels: usize,
}

impl Stream {
    fn open(pid: u32, rate: u32, channels: u16) -> Result<Stream> {
        let block = channels * 4;
        let format = WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: 0xFFFE, // WAVE_FORMAT_EXTENSIBLE
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
        // SAFETY: COM calls on interfaces we own; `format` outlives `Initialize`, which copies it.
        unsafe {
            let client = activate(pid)?;
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                10_000_000, // 1 s buffer: generous headroom against scheduling hiccups
                0,
                addr_of!(format).cast::<WAVEFORMATEX>(),
                None,
            )?;
            let event = CreateEventW(None, false, false, None)?;
            client.SetEventHandle(event)?;
            let capture: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            Ok(Stream { client, capture, event, channels: channels as usize })
        }
    }

    fn run(&self, stop: &AtomicBool, sink: &mut impl FnMut(CaptureEvent)) -> Result<()> {
        // SAFETY: the buffer returned by GetBuffer is valid for `frames * channels` floats until ReleaseBuffer.
        unsafe {
            while !stop.load(Ordering::Relaxed) {
                WaitForSingleObject(self.event, 100);
                while self.capture.GetNextPacketSize()? > 0 {
                    let (mut ptr, mut frames, mut flags) = (null_mut(), 0u32, 0u32);
                    self.capture.GetBuffer(&mut ptr, &mut frames, &mut flags, None, None)?;
                    let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || ptr.is_null();
                    let data = if silent {
                        Vec::new()
                    } else {
                        std::slice::from_raw_parts(ptr.cast::<f32>(), frames as usize * self.channels).to_vec()
                    };
                    self.capture.ReleaseBuffer(frames)?;
                    sink(CaptureEvent::Packet(Packet { data, frames: frames as usize, flags }));
                }
            }
        }
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: stopping our own client and closing our own event handle.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Signals an event when the asynchronous activation is done. Must be agile (called on any thread).
#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Completion(HANDLE);

impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(&self, _operation: Ref<IActivateAudioInterfaceAsyncOperation>) -> Result<()> {
        // SAFETY: the event handle stays open until `activate` has waited for it.
        unsafe { SetEvent(self.0) }
    }
}

impl IAgileObject_Impl for Completion_Impl {}

/// `PROPVARIANT` restricted to its `VT_BLOB` arm (same layout).
#[repr(C)]
struct BlobVariant {
    vt: u16,
    reserved: [u16; 3],
    size: u32,
    data: *const core::ffi::c_void,
}

fn activate(pid: u32) -> Result<IAudioClient> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let blob = BlobVariant { vt: VT_BLOB.0, reserved: [0; 3], size: size_of_val(&params) as u32, data: addr_of!(params).cast() };
    // SAFETY: `params`/`blob` live until activation completed (we wait for it); handles are closed below.
    unsafe {
        let done = CreateEventW(None, false, false, None)?;
        let handler: IActivateAudioInterfaceCompletionHandler = Completion(done).into();
        let result = (|| {
            let operation = ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                &IAudioClient::IID,
                Some(addr_of!(blob).cast::<PROPVARIANT>()),
                &handler,
            )?;
            if WaitForSingleObject(done, 10_000) != WAIT_OBJECT_0 {
                return Err(Error::new(E_FAIL, "process loopback activation timed out"));
            }
            let (mut hr, mut unknown): (HRESULT, Option<IUnknown>) = (HRESULT(0), None);
            operation.GetActivateResult(&mut hr, &mut unknown)?;
            hr.ok()?;
            unknown.ok_or_else(|| Error::from(E_NOINTERFACE))?.cast::<IAudioClient>()
        })();
        let _ = CloseHandle(done);
        result
    }
}

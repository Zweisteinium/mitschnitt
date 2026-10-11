//! Mitschnitt's own media session (SMTC).
//!
//! Hardware and Bluetooth media commands (headset buttons, earbud in-ear detection via AVRCP) don't arrive as
//! keys; Windows hands them to the *current* media session. While a track is recording, Mitschnitt presents
//! its own session as "playing": Windows makes it current within ~60 ms, so those commands land here - and
//! are ignored - instead of pausing the player. Closing it hands control straight back (measured 2026-10-10,
//! `mitschnitt smtc`).

use std::sync::{Arc, Mutex};

use windows::Foundation::TypedEventHandler;
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs,
};
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HICON, ICON_BIG, ICON_SMALL, MSG, PM_REMOVE, PeekMessageW,
    RegisterClassW, SendMessageW, TranslateMessage, WINDOW_EX_STYLE, WM_SETICON, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{HSTRING, Result, w};

/// Artwork when the track has no cover.
const LOGO: &[u8] = include_bytes!("../../../app/src-tauri/icons/128x128@2x.png");

/// A registered media session; removed when dropped.
pub struct OwnSession {
    smtc: SystemMediaTransportControls,
    hwnd: HWND,
    token: i64,
    /// Buttons pressed on this session (for diagnostics).
    pub pressed: Arc<Mutex<Vec<String>>>,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: default handling for our hidden window.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

impl OwnSession {
    /// Register a session showing `title` / `artist` and `cover` (JPEG/PNG; the logo when `None`).
    /// Desktop apps need a window to own it; a hidden one is enough.
    pub fn start(title: &str, artist: &str, cover: Option<&[u8]>) -> Result<OwnSession> {
        // SAFETY: plain Win32 window creation; the class name and procedure live for the whole process.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: w!("MitschnittSession"), ..Default::default() };
            RegisterClassW(&class); // fails harmlessly if already registered
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("MitschnittSession"), w!("Mitschnitt"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(instance.into()), None)?;
            // The executable's icon for the window that owns the session.
            if let Ok(exe) = std::env::current_exe() {
                let (mut big, mut small) = (HICON::default(), HICON::default());
                if ExtractIconExW(&HSTRING::from(exe.as_os_str()), 0, Some(&mut big), Some(&mut small), 1) > 0 {
                    SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(big.0 as isize)));
                    SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_SMALL as usize)), Some(LPARAM(small.0 as isize)));
                }
            }
            hwnd
        };
        let interop = windows::core::factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()?;
        // SAFETY: `hwnd` is our live window.
        let smtc: SystemMediaTransportControls = unsafe { interop.GetForWindow(hwnd)? };
        smtc.SetIsEnabled(true)?;
        for set in [
            SystemMediaTransportControls::SetIsPlayEnabled,
            SystemMediaTransportControls::SetIsPauseEnabled,
            SystemMediaTransportControls::SetIsStopEnabled,
            SystemMediaTransportControls::SetIsNextEnabled,
            SystemMediaTransportControls::SetIsPreviousEnabled,
        ] {
            set(&smtc, true)?;
        }
        let updater = smtc.DisplayUpdater()?;
        updater.SetType(MediaPlaybackType::Music)?;
        updater.MusicProperties()?.SetTitle(&HSTRING::from(title))?;
        updater.MusicProperties()?.SetArtist(&HSTRING::from(artist))?;
        if let Ok(art) = artwork(cover.unwrap_or(LOGO)) {
            updater.SetThumbnail(&art)?;
        }
        updater.Update()?;

        let pressed = Arc::new(Mutex::new(Vec::new()));
        let log = pressed.clone();
        let token = smtc.ButtonPressed(&TypedEventHandler::<SystemMediaTransportControls, SystemMediaTransportControlsButtonPressedEventArgs>::new(
            move |_, args| {
                if let Some(args) = args.as_ref() {
                    let name = match args.Button()? {
                        SystemMediaTransportControlsButton::Play => "play",
                        SystemMediaTransportControlsButton::Pause => "pause",
                        SystemMediaTransportControlsButton::Stop => "stop",
                        SystemMediaTransportControlsButton::Next => "next",
                        SystemMediaTransportControlsButton::Previous => "previous",
                        _ => "other",
                    };
                    if let Ok(mut l) = log.lock() {
                        l.push(name.to_owned());
                    }
                }
                Ok(())
            },
        ))?;
        smtc.SetPlaybackStatus(MediaPlaybackStatus::Playing)?;
        Ok(OwnSession { smtc, hwnd, token, pressed })
    }

    pub fn set_playing(&self, playing: bool) -> Result<()> {
        self.smtc.SetPlaybackStatus(if playing { MediaPlaybackStatus::Playing } else { MediaPlaybackStatus::Paused })
    }

    /// Handle pending window messages (call regularly on the thread that created the session).
    pub fn pump(&self) {
        let mut msg = MSG::default();
        // SAFETY: standard message pump for our own thread.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    pub fn take_pressed(&self) -> Vec<String> {
        self.pressed.lock().map(|mut l| std::mem::take(&mut *l)).unwrap_or_default()
    }
}

/// An in-memory image as SMTC artwork.
fn artwork(bytes: &[u8]) -> Result<RandomAccessStreamReference> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(bytes)?;
    writer.StoreAsync()?.join()?;
    writer.FlushAsync()?.join()?;
    writer.DetachStream()?;
    stream.Seek(0)?;
    RandomAccessStreamReference::CreateFromStream(&stream)
}

impl Drop for OwnSession {
    fn drop(&mut self) {
        let _ = self.smtc.RemoveButtonPressed(self.token);
        let _ = self.smtc.SetPlaybackStatus(MediaPlaybackStatus::Closed);
        let _ = self.smtc.SetIsEnabled(false);
        // SAFETY: destroying our own window.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

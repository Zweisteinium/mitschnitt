//! Media-key guard: while a track is recording, media keys (play/pause, stop, next, previous) from keyboards
//! and headsets are swallowed system-wide, so nothing but the player's own pause button can interrupt it.
//!
//! A low-level keyboard hook (`WH_KEYBOARD_LL`) sees keys before Windows hands them to any app. The callback
//! only compares the virtual-key code - every other key passes through untouched, nothing is stored. It
//! runs on its own thread with a message loop (required for LL hooks) and must stay fast: Windows silently
//! removes hooks that take longer than `LowLevelHooksTimeout`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
};

const VK_MEDIA_NEXT: u32 = 0xB0;
const VK_MEDIA_PLAY_PAUSE: u32 = 0xB3;

/// Swallow media keys right now (set while a track is recording).
static ARMED: AtomicBool = AtomicBool::new(false);
/// Key presses swallowed since the guard started.
static BLOCKED: AtomicU32 = AtomicU32::new(0);
/// Diagnostics: media and volume keys seen (vk codes), when logging.
static LOG: Mutex<Option<Vec<u32>>> = Mutex::new(None);

fn is_media(vk: u32) -> bool {
    (VK_MEDIA_NEXT..=VK_MEDIA_PLAY_PAUSE).contains(&vk)
}

/// Keys that are logged in diagnostics mode: browser, volume, media and app-launch keys - never typing keys.
fn is_loggable(vk: u32) -> bool {
    (0xA6..=0xB7).contains(&vk)
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lparam points to a KBDLLHOOKSTRUCT.
        let vk = unsafe { (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode };
        let down = wparam.0 == WM_KEYDOWN as usize || wparam.0 == WM_SYSKEYDOWN as usize;
        if down && is_loggable(vk) {
            if let Ok(mut log) = LOG.try_lock() {
                if let Some(log) = log.as_mut() {
                    log.push(vk);
                }
            }
        }
        if is_media(vk) && ARMED.load(Ordering::Relaxed) {
            if down {
                BLOCKED.fetch_add(1, Ordering::Relaxed);
            }
            return LRESULT(1); // swallow both press and release
        }
    }
    // SAFETY: passing the event on to the next hook.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// The installed hook; removed when dropped. One per process.
pub struct KeyGuard {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl KeyGuard {
    pub fn start() -> Result<KeyGuard, String> {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("keys".into())
            .spawn(move || {
                // SAFETY: the hook procedure is a plain function living as long as the process; the message loop
                // runs on this thread until WM_QUIT, then the hook is removed on the same thread.
                unsafe {
                    let module = GetModuleHandleW(None).ok();
                    let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), module.map(Into::into), 0);
                    let _ = tx.send(hook.as_ref().map(|_| GetCurrentThreadId()).map_err(|e| e.to_string()));
                    let Ok(hook) = hook else { return };
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        DispatchMessageW(&msg);
                    }
                    let _ = UnhookWindowsHookEx(hook);
                }
            })
            .map_err(|e| e.to_string())?;
        let thread_id = rx.recv().map_err(|e| e.to_string())??;
        BLOCKED.store(0, Ordering::Relaxed);
        Ok(KeyGuard { thread_id, thread: Some(thread) })
    }

    /// Swallow media keys (true while a track is recording).
    pub fn arm(&self, armed: bool) {
        ARMED.store(armed, Ordering::Relaxed);
    }

    /// Key presses swallowed so far.
    pub fn blocked(&self) -> u32 {
        BLOCKED.load(Ordering::Relaxed)
    }

    /// Diagnostics: start collecting media/volume keys seen (see [`KeyGuard::take_log`]).
    pub fn log(&self, on: bool) {
        if let Ok(mut log) = LOG.lock() {
            *log = on.then(Vec::new);
        }
    }

    pub fn take_log(&self) -> Vec<u32> {
        LOG.lock().ok().and_then(|mut l| l.as_mut().map(std::mem::take)).unwrap_or_default()
    }
}

impl Drop for KeyGuard {
    fn drop(&mut self) {
        ARMED.store(false, Ordering::Relaxed);
        // SAFETY: asking our own hook thread to leave its message loop.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Readable name of a logged key.
pub fn key_name(vk: u32) -> &'static str {
    match vk {
        0xAD => "volume mute",
        0xAE => "volume down",
        0xAF => "volume up",
        0xB0 => "next track",
        0xB1 => "previous track",
        0xB2 => "stop",
        0xB3 => "play/pause",
        0xA6..=0xAC => "browser key",
        _ => "app key",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_media_keys_are_guarded() {
        assert!((0xB0..=0xB3).all(is_media));
        assert!(!is_media(0xAF), "volume keys stay usable");
        assert!(!is_media(0x20), "typing is never touched");
        assert!(!is_loggable(0x41), "letters are never logged");
    }
}

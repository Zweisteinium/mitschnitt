use windows::Win32::System::Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState};

/// Keeps the computer awake (not the display) until dropped, on the thread that created it.
pub struct KeepAwake;

impl KeepAwake {
    pub fn start() -> KeepAwake {
        // SAFETY: plain flag call, affects only this thread's request.
        unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
        KeepAwake
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        // SAFETY: as above; clears this thread's request.
        unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
    }
}

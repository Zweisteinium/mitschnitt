/// Open a link with its registered app (`spotify:track:…:play` starts that track in the Spotify desktop app).
pub fn open_link(link: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, w};
    // SAFETY: plain shell call with valid, NUL-terminated strings.
    let result = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(link), None, None, SW_SHOWNORMAL) };
    // ShellExecute reports success as a value above 32.
    if result.0 as usize > 32 { Ok(()) } else { Err(format!("can't open {link} (error {})", result.0 as usize)) }
}

/// How an app's window was before a link was opened for it.
#[derive(Clone, Copy, PartialEq)]
enum WindowState {
    NotRunning,
    /// Running without a visible window (in the tray).
    Hidden,
    Minimized,
    Visible,
}

/// Open a link for an app that runs in the background (`exe`, e.g. "spotify"). Spotify brings its window to the front
/// for every link (sometimes twice while it starts), and Windows lets no other program prevent that. So for a few
/// seconds, whenever it takes the front, its window goes back to how it was (minimized, hidden, or visible behind) and
/// the focus goes back to the window the user was in. Nothing is touched when the user was in the app itself.
pub fn open_link_in_background(link: &str, exe: &str) -> Result<(), String> {
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsIconic, SW_HIDE, SW_MINIMIZE, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, ShowWindow};

    // SAFETY: plain query of the foreground window.
    let before = unsafe { GetForegroundWindow() };
    let pids = app_pids(exe);
    if pids.is_empty() || !window_pid(before).is_some_and(|pid| pids.contains(&pid)) {
        let state = match main_window(&pids) {
            _ if pids.is_empty() => WindowState::NotRunning,
            None => WindowState::Hidden,
            // SAFETY: a window handle from the system.
            Some(w) if unsafe { IsIconic(w) }.as_bool() => WindowState::Minimized,
            Some(_) => WindowState::Visible,
        };
        let (before, exe) = (before.0 as isize, exe.to_owned()); // a window handle isn't Send; its value is
        std::thread::spawn(move || {
            let before = HWND(before as _);
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut seen = HWND::default();
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(15));
                // SAFETY: plain query of the foreground window.
                let front = unsafe { GetForegroundWindow() };
                if front == seen || front == before {
                    continue;
                }
                seen = front;
                if !window_pid(front).is_some_and(|pid| app_pids(&exe).contains(&pid)) {
                    continue;
                }
                // SAFETY: window handles from the system; every call fails harmlessly for a window that is gone.
                unsafe {
                    match state {
                        WindowState::NotRunning | WindowState::Minimized => _ = ShowWindow(front, SW_SHOWMINNOACTIVE),
                        WindowState::Hidden => _ = ShowWindow(front, SW_HIDE),
                        WindowState::Visible => {}
                    }
                    if !activate(before, front) && state == WindowState::Visible {
                        // Windows refused: minimizing activates the window below, then the app comes back unfocused.
                        let _ = ShowWindow(front, SW_MINIMIZE);
                        let _ = ShowWindow(front, SW_SHOWNOACTIVATE);
                    }
                }
            }
        });
    }
    open_link(link)
}

/// Give `target` the focus that `front` holds. Windows only lets the foreground's own input queue do that, so the
/// calling thread shares it for the moment.
fn activate(target: windows::Win32::Foundation::HWND, front: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, SetForegroundWindow};
    if target.is_invalid() {
        return false;
    }
    // SAFETY: thread ids and window handles from the system; the input queues are detached again right after.
    unsafe {
        let (ours, theirs) = (GetCurrentThreadId(), GetWindowThreadProcessId(front, None));
        let attached = AttachThreadInput(ours, theirs, true).as_bool();
        let done = SetForegroundWindow(target).as_bool();
        if attached {
            let _ = AttachThreadInput(ours, theirs, false);
        }
        done
    }
}

fn window_pid(window: windows::Win32::Foundation::HWND) -> Option<u32> {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    if window.is_invalid() {
        return None;
    }
    let mut pid = 0;
    // SAFETY: valid out pointer for the process id.
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    Some(pid)
}

fn app_pids(exe: &str) -> Vec<u32> {
    crate::sessions::processes().into_iter().filter(|p| p.name.to_lowercase().trim_end_matches(".exe") == exe).map(|p| p.pid).collect()
}

/// The app's main window: a visible top-level window with a title.
fn main_window(pids: &[u32]) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextLengthW, IsWindowVisible};
    use windows::core::BOOL;

    struct Search<'a> {
        pids: &'a [u32],
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(window: HWND, search: LPARAM) -> BOOL {
        // SAFETY: `search` is the `Search` passed to EnumWindows below, alive for the whole enumeration.
        let search = unsafe { &mut *(search.0 as *mut Search) };
        // SAFETY: a window handle from the system.
        let main = unsafe { IsWindowVisible(window).as_bool() && GetWindowTextLengthW(window) > 0 };
        if main && window_pid(window).is_some_and(|pid| search.pids.contains(&pid)) {
            search.found = Some(window);
            return false.into();
        }
        true.into()
    }
    let mut search = Search { pids, found: None };
    // SAFETY: the callback only uses `search` while EnumWindows runs; stopping early is reported as an error.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut search as *mut Search as isize)) };
    search.found
}

/// Show a file in Explorer, or open a folder or web link.
pub fn open(target: &str) -> Result<(), String> {
    // Explorer takes a long link with a query for a path and opens a folder window instead of the browser.
    if target.contains("://") {
        return open_link(target);
    }
    let arg = if std::path::Path::new(target).is_file() { format!("/select,{target}") } else { target.to_owned() };
    std::process::Command::new("explorer").arg(arg).spawn().map(|_| ()).map_err(|e| e.to_string())
}

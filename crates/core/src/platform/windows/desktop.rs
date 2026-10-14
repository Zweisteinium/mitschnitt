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

/// For an app running in the background (`exe`). Spotify brings its window to the front for every link (sometimes
/// twice while it starts), and Windows lets no other program prevent that. So for a few seconds the focus goes
/// straight back to the window the user was in, and a window that was minimized (or not running) is minimized
/// again via its own minimize command. (Moving another app's window from outside, in the middle of its own show,
/// left Spotify off-screen and unrestorable.)
pub fn open_link_in_background(link: &str, exe: &str) -> Result<(), String> {
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsIconic, PostMessageW, SC_MINIMIZE, WM_SYSCOMMAND};

    // SAFETY: plain query of the foreground window.
    let before = unsafe { GetForegroundWindow() };
    let pids = app_pids(exe);
    if !pids.is_empty() && window_pid(before).is_some_and(|pid| pids.contains(&pid)) {
        crate::log!("focus: {exe} is in front already");
        return open_link(link);
    }
    // SAFETY: a window handle from the system.
    let minimize_after = main_window(&pids).is_none_or(|w| unsafe { IsIconic(w) }.as_bool());
    // A starting app can take several seconds to show its window.
    let watch = if pids.is_empty() { Duration::from_secs(20) } else { Duration::from_secs(3) };
    crate::log!("focus: front is {}, {exe} running={} minimize_after={minimize_after}", who(before), !pids.is_empty());
    let (before, exe) = (before.0 as isize, exe.to_owned()); // a window handle isn't Send; its value is
    std::thread::spawn(move || {
        let before = HWND(before as _);
        let deadline = Instant::now() + watch;
        let minimized = |window: HWND| unsafe { IsIconic(window) }.as_bool(); // SAFETY: a window handle from the system
        // SAFETY: posting a standard command to a window from the system; a closed window ignores it.
        let minimize = |window: HWND| unsafe { _ = PostMessageW(Some(window), WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0)) };
        // Windows may refuse the focus while the app is still activating itself, so it is handed back on every round
        // for a moment; after that it stays (the user may have clicked it).
        let mut grabbed: Option<Instant> = None;
        let mut last_minimize = Instant::now() - Duration::from_secs(1);
        let started = Instant::now();
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(15));
            // SAFETY: plain query of the foreground window.
            let front = unsafe { GetForegroundWindow() };
            if front == before || !window_pid(front).is_some_and(|pid| app_pids(&exe).contains(&pid)) {
                grabbed = None;
                continue;
            }
            if grabbed.is_none() {
                crate::log!("focus: {exe} took the front after {} ms", started.elapsed().as_millis());
            }
            if grabbed.get_or_insert_with(Instant::now).elapsed() >= Duration::from_millis(1_500) {
                continue;
            }
            activate(before, front);
            // Minimizing the active window also makes Windows activate the previous one, when it refused above.
            if minimize_after && last_minimize.elapsed() >= Duration::from_millis(100) {
                last_minimize = Instant::now();
                if let Some(window) = main_window(&app_pids(&exe)).filter(|w| !minimized(*w)) {
                    minimize(window);
                }
            }
        }
        // It may have shown itself again after the last grab.
        if minimize_after
            && let Some(window) = main_window(&app_pids(&exe)).filter(|w| !minimized(*w))
        {
            minimize(window);
        }
        // SAFETY: plain query of the foreground window.
        let front = unsafe { GetForegroundWindow() };
        crate::log!("focus: watch over, front is {}{}", who(front), if front == before { " (as before)" } else { "" });
    });
    open_link(link)
}

/// "firefox.exe (pid 8812)", for the log.
fn who(window: windows::Win32::Foundation::HWND) -> String {
    let Some(pid) = window_pid(window) else { return "nothing".into() };
    let name = crate::sessions::processes().into_iter().find(|p| p.pid == pid).map(|p| p.name).unwrap_or_default();
    format!("{name} (pid {pid})")
}

/// Windows only lets the foreground's own input queue hand over the focus, so the calling thread shares it briefly.
fn activate(target: windows::Win32::Foundation::HWND, front: windows::Win32::Foundation::HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, SetForegroundWindow};
    if target.is_invalid() {
        return;
    }
    // SAFETY: thread ids and window handles from the system; the input queues are detached again right after.
    unsafe {
        let (ours, theirs) = (GetCurrentThreadId(), GetWindowThreadProcessId(front, None));
        let attached = AttachThreadInput(ours, theirs, true).as_bool();
        let _ = SetForegroundWindow(target);
        if attached {
            let _ = AttachThreadInput(ours, theirs, false);
        }
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

/// A visible top-level window with a title.
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
    use std::os::windows::process::CommandExt;
    // Raw: Rust quotes an argument with spaces as a whole, and Explorer ignores a quoted "/select,…".
    let arg = if std::path::Path::new(target).is_file() { format!("/select,\"{target}\"") } else { format!("\"{target}\"") };
    std::process::Command::new("explorer").raw_arg(arg).spawn().map(|_| ()).map_err(|e| e.to_string())
}

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask};
use x11rb::rust_connection::RustConnection;

/// Open a link with its registered app (`spotify:track:…:play` starts that track in Spotify).
pub fn open_link(link: &str) -> Result<(), String> {
    xdg_open(link.as_ref())
}

/// Open a link for an app that runs in the background (`exe`). Not over MPRIS: Spotify's `OpenUri` starts the song,
/// then restarts it from 0:00 about 0.3 s later (measured with 1.2.96), which puts the start into the recording twice.
///
/// Spotify takes the focus for every link, and like on Windows it goes straight back for a few seconds. Spotify is an
/// X11 window (XWayland on a Wayland desktop), so this uses the standard X11 calls: minimizing hands the focus back to
/// the window before, mapping it again shows it behind that one, without the focus (lowering kept the focus, KWin
/// 6.8). A window that was minimized (or not running) stays minimized. An app with a native Wayland window can't
/// be reached, and the desktop decides.
pub fn open_link_in_background(link: &str, exe: &str) -> Result<(), String> {
    let Some(x) = X::connect() else {
        crate::log!("focus: no X11 display, {exe} may come to the front");
        return open_link(link);
    };
    let window = x.app_window(exe);
    if window.is_some() && x.active() == window {
        crate::log!("focus: {exe} is in front already");
        return open_link(link);
    }
    let minimize_after = window.is_none_or(|w| x.minimized(w));
    // A starting app can take several seconds to show its window.
    let watch = if window.is_none() { Duration::from_secs(20) } else { Duration::from_secs(3) };
    crate::log!("focus: {exe} window={} minimize_after={minimize_after}", window.is_some());
    let exe = exe.to_owned();
    std::thread::spawn(move || hand_back(&x, &exe, window, minimize_after, watch));
    open_link(link)
}

fn hand_back(x: &X, exe: &str, mut window: Option<u32>, minimize_after: bool, watch: Duration) {
    let started = Instant::now();
    // Handed back on every round for a moment; after that it stays (the user may have clicked it).
    let mut grabbed: Option<Instant> = None;
    while started.elapsed() < watch {
        std::thread::sleep(Duration::from_millis(15));
        window = window.or_else(|| x.app_window(exe));
        let Some(w) = window else { continue };
        if x.active() != Some(w) {
            grabbed = None;
            continue;
        }
        if grabbed.is_none() {
            crate::log!("focus: {exe} took the front after {} ms", started.elapsed().as_millis());
        }
        if grabbed.get_or_insert_with(Instant::now).elapsed() >= Duration::from_millis(1_500) {
            continue;
        }
        x.minimize(w);
        if !minimize_after {
            // Mapped before the window manager has minimized it, it would stay minimized.
            let asked = Instant::now();
            while !x.minimized(w) && asked.elapsed() < Duration::from_millis(500) {
                std::thread::sleep(Duration::from_millis(10));
            }
            x.restore(w);
        }
    }
    // It may have shown itself again after the last grab.
    if minimize_after && let Some(w) = window.or_else(|| x.app_window(exe)).filter(|w| !x.minimized(*w)) {
        x.minimize(w);
    }
    crate::log!("focus: watch over, {exe} {}", if window.is_some_and(|w| x.active() == Some(w)) { "in front" } else { "not in front" });
}

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST,
        _NET_WM_PID,
        WM_STATE,
        WM_CHANGE_STATE,
    }
}

/// ICCCM's `IconicState`, in `WM_STATE` and asked for with `WM_CHANGE_STATE`.
const ICONIC: u32 = 3;

struct X {
    conn: RustConnection,
    root: u32,
    atoms: Atoms,
}

impl X {
    fn connect() -> Option<X> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let atoms = Atoms::new(&conn).ok()?.reply().ok()?;
        Some(X { conn, root, atoms })
    }

    fn property(&self, window: u32, property: impl Into<u32>, kind: impl Into<u32>) -> Vec<u32> {
        let reply = self.conn.get_property(false, window, property, kind, 0, 4096).ok().and_then(|c| c.reply().ok());
        reply.and_then(|r| r.value32().map(Iterator::collect)).unwrap_or_default()
    }

    /// The focused X11 window; none while a Wayland window has the focus.
    fn active(&self) -> Option<u32> {
        self.property(self.root, self.atoms._NET_ACTIVE_WINDOW, AtomEnum::WINDOW).first().copied().filter(|w| *w != 0)
    }

    /// By its process, or by its class where the pid is another namespace's (Flatpak, Snap).
    fn app_window(&self, exe: &str) -> Option<u32> {
        let pids: Vec<u32> = crate::sessions::processes().into_iter().filter(|p| p.name.eq_ignore_ascii_case(exe)).map(|p| p.pid).collect();
        self.property(self.root, self.atoms._NET_CLIENT_LIST, AtomEnum::WINDOW).into_iter().find(|w| {
            self.property(*w, self.atoms._NET_WM_PID, AtomEnum::CARDINAL).first().is_some_and(|pid| pids.contains(pid)) || {
                let class = self.conn.get_property(false, *w, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256).ok().and_then(|c| c.reply().ok());
                class.is_some_and(|c| c.value.split(|b| *b == 0).any(|name| name.eq_ignore_ascii_case(exe.as_bytes())))
            }
        })
    }

    fn minimized(&self, window: u32) -> bool {
        self.property(window, self.atoms.WM_STATE, self.atoms.WM_STATE).first() == Some(&ICONIC)
    }

    fn minimize(&self, window: u32) {
        let event = ClientMessageEvent::new(32, window, self.atoms.WM_CHANGE_STATE, [ICONIC, 0, 0, 0, 0]);
        let _ = self.conn.send_event(false, self.root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, event);
        let _ = self.conn.flush();
    }

    fn restore(&self, window: u32) {
        let _ = self.conn.map_window(window);
        let _ = self.conn.flush();
    }
}

/// Show a file in the file manager, or open a folder or web link.
pub fn open(target: &str) -> Result<(), String> {
    let path = Path::new(target);
    if !path.is_file() {
        return xdg_open(target.as_ref());
    }
    show_item(path).or_else(|_| xdg_open(path.parent().unwrap_or(path).as_os_str()))
}

fn show_item(path: &Path) -> Result<(), String> {
    let uri = file_uri(&std::fs::canonicalize(path).map_err(|e| e.to_string())?);
    let manager = "org.freedesktop.FileManager1";
    super::bus()?.call_method(Some(manager), "/org/freedesktop/FileManager1", Some(manager), "ShowItems", &(vec![uri], "")).map(|_| ()).map_err(|e| e.to_string())
}

fn xdg_open(target: &OsStr) -> Result<(), String> {
    Command::new("xdg-open").arg(target).spawn().map(|_| ()).map_err(|e| format!("xdg-open: {e}"))
}

fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for b in path.as_os_str().as_encoded_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => uri.push(*b as char),
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    #[test]
    fn file_uris_are_percent_encoded() {
        assert_eq!(super::file_uri("/home/a/My Music/ö.flac".as_ref()), "file:///home/a/My%20Music/%C3%B6.flac");
    }
}

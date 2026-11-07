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
/// then restarts it from 0:00 about 0.3 s later (measured with 1.2.96; 1.3.4 jumps about too and takes the focus).
///
/// Spotify takes the focus for every link, and like on Windows it goes straight back to the window before for a few
/// seconds; a window that was minimized (or not running) is minimized again. On a Wayland desktop only the
/// compositor may focus a window: KDE's KWin through a short script, elsewhere it is given back where X11 can reach
/// (Spotify is an X11 window, through XWayland there).
pub fn open_link_in_background(link: &str, exe: &str) -> Result<(), String> {
    let running = crate::sessions::find_app(exe, &crate::sessions::processes()).is_some();
    // A starting app can take several seconds to show its window.
    let watch = if running { Duration::from_secs(3) } else { Duration::from_secs(20) };
    match kwin::hand_back(exe, watch) {
        Ok(()) => {
            crate::log!("focus: KWin hands it back from {exe} for {} s", watch.as_secs());
            return open_link(link);
        }
        Err(e) => crate::log!("focus: no KWin ({e}), X11"),
    }
    let Some(x) = X::connect() else {
        crate::log!("focus: no X11 display, {exe} may come to the front");
        return open_link(link);
    };
    let window = x.app_window(exe);
    let before = x.active();
    if window.is_some() && before == window {
        crate::log!("focus: {exe} is in front already");
        return open_link(link);
    }
    let minimize_after = window.is_none_or(|w| x.minimized(w));
    crate::log!("focus: {exe} window={} minimize_after={minimize_after} before={}", window.is_some(), before.is_some());
    let exe = exe.to_owned();
    std::thread::spawn(move || hand_back(&x, &exe, window, before, minimize_after, watch));
    open_link(link)
}

/// KWin's scripting, over D-Bus: the script sees every window, Wayland's too, and may focus one.
mod kwin {
    use std::time::Duration;

    const SCRIPT: &str = r#"
const app = (w) => w && w.resourceClass.toLowerCase() == "EXE";
const before = workspace.activeWindow;
if (before && !app(before)) {
    const window = workspace.windowList().find(app);
    const minimize = !window || window.minimized;
    workspace.windowActivated.connect((w) => {
        if (!app(w)) return;
        // In this order: minimizing the active window would focus another one first.
        if (workspace.windowList().includes(before)) workspace.activeWindow = before;
        if (minimize) w.minimized = true;
    });
}
"#;

    /// Loaded until `watch` is over; an app already in front is left there.
    pub fn hand_back(exe: &str, watch: Duration) -> Result<(), String> {
        let bus = super::super::bus()?;
        let name = format!("mitschnitt-focus-{}", std::process::id());
        let path = std::env::temp_dir().join(format!("{name}.js"));
        std::fs::write(&path, SCRIPT.replace("EXE", &exe.to_lowercase())).map_err(|e| e.to_string())?;
        // An earlier link's script may still be loaded.
        let _ = bus.call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "unloadScript", &(name.as_str(),));
        let reply = bus
            .call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "loadScript", &(path.to_string_lossy().as_ref(), name.as_str()))
            .map_err(|e| e.to_string())?;
        let id: i32 = reply.body().deserialize().map_err(|e| e.to_string())?;
        if id < 0 {
            return Err("script not loaded".into());
        }
        bus.call_method(Some("org.kde.KWin"), format!("/Scripting/Script{id}").as_str(), Some("org.kde.kwin.Script"), "run", &()).map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            std::thread::sleep(watch);
            let _ = bus.call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "unloadScript", &(name.as_str(),));
            let _ = std::fs::remove_file(path);
        });
        Ok(())
    }
}

/// X11: the window before gets the focus back by the standard request; when only a Wayland window had it (no
/// compositor to ask), minimizing hands the focus on and mapping again shows the app behind, without the focus.
fn hand_back(x: &X, exe: &str, mut window: Option<u32>, before: Option<u32>, minimize_after: bool, watch: Duration) {
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
        if minimize_after {
            x.minimize(w);
        }
        match before {
            Some(before) => x.activate(before),
            None if !minimize_after => {
                x.minimize(w);
                // Mapped before the window manager has minimized it, it would stay minimized.
                let asked = Instant::now();
                while !x.minimized(w) && asked.elapsed() < Duration::from_millis(500) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                x.restore(w);
            }
            None => {}
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

    /// As a pager asks for it (source 2), which window managers grant.
    fn activate(&self, window: u32) {
        let event = ClientMessageEvent::new(32, window, self.atoms._NET_ACTIVE_WINDOW, [2, 0, 0, 0, 0]);
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

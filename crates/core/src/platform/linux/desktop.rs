use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

/// Set by the AppImage runtime for its bundled libraries; they must not leak into other programs.
const APPIMAGE_ENV: &[&str] = &[
    "LD_LIBRARY_PATH",
    "GDK_PIXBUF_MODULE_FILE",
    "GDK_PIXBUF_MODULEDIR",
    "GIO_MODULE_DIR",
    "GIO_EXTRA_MODULES",
    "GSETTINGS_SCHEMA_DIR",
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GST_PLUGIN_SCANNER",
    "GTK_PATH",
    "GTK_EXE_PREFIX",
    "GTK_DATA_PREFIX",
    "GTK_IM_MODULE_FILE",
];

/// Open a link with its registered app (`spotify:track:…:play` starts that track in Spotify).
pub fn open_link(link: &str) -> Result<(), String> {
    xdg_open(link.as_ref())
}

/// Open a link for an app that runs in the background. Not over MPRIS: Spotify's `OpenUri` starts the song, then
/// restarts it from 0:00 about 0.3 s later (measured with 1.2.96), which puts the start into the recording twice.
pub fn open_link_in_background(link: &str, _exe: &str) -> Result<(), String> {
    open_link(link)
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
    let mut command = Command::new("xdg-open");
    command.arg(target);
    if std::env::var_os("APPIMAGE").is_some() {
        APPIMAGE_ENV.iter().for_each(|var| _ = command.env_remove(var));
    }
    command.spawn().map(|_| ()).map_err(|e| format!("xdg-open: {e}"))
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

//! MPRIS over the session bus.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zbus::blocking::fdo::{DBusProxy, PropertiesProxy};
use zbus::names::InterfaceName;
use zbus::zvariant::{OwnedValue, Value};

use crate::flac::Cover;
use crate::media::{Control, Extra, Source};
use crate::passive::{Media, Track};
use crate::profile::{PROFILES, Profile};
use crate::sessions;

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const MAX_COVER: u64 = 16 << 20;
/// A length change with the old title: wait this long for the new title (same rule as on Windows).
const SETTLE: Duration = Duration::from_millis(1_500);

pub struct Session(PropertiesProxy<'static>);

impl Session {
    fn open(name: String) -> Result<Session, String> {
        let proxy = PropertiesProxy::builder(super::bus()?).destination(name).and_then(|b| b.path("/org/mpris/MediaPlayer2")).and_then(|b| b.build());
        proxy.map(Session).map_err(|e| e.to_string())
    }

    fn props(&self) -> Result<Props, String> {
        self.0.get_all(InterfaceName::from_static_str_unchecked("org.mpris.MediaPlayer2.Player")).map(Props).map_err(|e| e.to_string())
    }
}

fn players() -> Result<Vec<String>, String> {
    let names = DBusProxy::new(super::bus()?).and_then(|d| Ok(d.list_names()?)).map_err(|e| e.to_string())?;
    Ok(names.into_iter().map(|n| n.to_string()).filter(|n| n.starts_with(PREFIX)).collect())
}

fn player_of(profile: &Profile, players: &[String]) -> Option<String> {
    players.iter().find(|n| profile.matches_media(&n[PREFIX.len()..])).cloned()
}

pub fn sources() -> Result<Vec<Source>, String> {
    let procs = sessions::processes();
    let players = players()?;
    Ok(PROFILES
        .iter()
        .map(|p| {
            let mut source = Source { id: p.id, name: p.name, running: false, playing: false, title: String::new(), artist: String::new() };
            source.running = procs.iter().any(|proc| p.matches_exe(&proc.name));
            let props = player_of(p, &players).filter(|_| source.running).and_then(|n| Session::open(n).and_then(|s| s.props()).ok());
            if let Some(props) = props {
                source.playing = props.playing();
                if let Some(t) = props.track() {
                    (source.title, source.artist) = (t.title, t.artist);
                }
            }
            source
        })
        .collect())
}

pub fn find(profile: &Profile) -> Result<Option<Session>, String> {
    player_of(profile, &players()?).map(Session::open).transpose()
}

pub struct Watcher {
    session: Session,
    track: Option<Track>,
    unsettled: Option<(Instant, Track)>,
}

impl Watcher {
    pub fn new(session: Session) -> Watcher {
        Watcher { session, track: None, unsettled: None }
    }

    pub fn snapshot(&mut self) -> Result<Media, String> {
        let props = self.session.props()?;
        let track = props.track();
        if let (Some(old), Some(new)) = (&self.track, &track)
            && old.duration_ms != new.duration_ms && (&old.title, &old.artist) == (&new.title, &new.artist)
        {
            self.unsettled.get_or_insert((Instant::now(), old.clone()));
        }
        if let Some((since, old)) = &self.unsettled
            && (track.as_ref().is_none_or(|t| (&t.title, &t.artist) != (&old.title, &old.artist)) || since.elapsed() >= SETTLE)
        {
            self.unsettled = None;
        }
        self.track = track;
        let track = self.track.clone().filter(|t| self.unsettled.is_none() && !t.title.is_empty() && t.duration_ms > 0);
        Ok(Media { playing: props.playing(), track, position_ms: props.position_ms() })
    }

    /// Send a command to the player.
    pub fn control(&self, control: Control) -> Result<(), String> {
        let method = match control {
            Control::Pause => "Pause",
            Control::Next => "Next",
            Control::Previous => "Previous",
        };
        let destination = self.session.0.inner().destination().to_string();
        super::bus()?
            .call_method(Some(destination.as_str()), "/org/mpris/MediaPlayer2", Some("org.mpris.MediaPlayer2.Player"), method, &())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Album artist, track number and cover of what is playing now (best effort).
    pub fn extra(&self) -> Extra {
        let Ok(props) = self.session.props() else { return Extra::default() };
        let metadata = props.metadata();
        Extra {
            album_artist: metadata.text("xesam:albumArtist"),
            track_number: metadata.int("xesam:trackNumber").unwrap_or(0).max(0) as u32,
            cover: cover(&metadata.text("mpris:artUrl")),
        }
    }
}

struct Props(HashMap<String, OwnedValue>);

impl Props {
    fn playing(&self) -> bool {
        self.0.get("PlaybackStatus").is_some_and(|v| text(v) == "Playing")
    }

    fn position_ms(&self) -> i64 {
        self.0.get("Position").and_then(|v| int(v)).unwrap_or(0) / 1000
    }

    fn metadata(&self) -> Metadata<'_> {
        let mut map = HashMap::new();
        if let Some(Value::Dict(dict)) = self.0.get("Metadata").map(|v| inner(v)) {
            for (k, v) in dict.iter() {
                if let Value::Str(k) = inner(k) {
                    map.insert(k.to_string(), inner(v).clone());
                }
            }
        }
        Metadata(map)
    }

    /// `None` for ads.
    fn track(&self) -> Option<Track> {
        let m = self.metadata();
        if is_ad(&m.text("mpris:trackid")) {
            return None;
        }
        let duration_ms = m.int("mpris:length").unwrap_or(0).max(0) as u64 / 1000;
        Some(Track { title: m.text("xesam:title"), artist: m.text("xesam:artist"), album: m.text("xesam:album"), duration_ms })
    }
}

struct Metadata<'a>(HashMap<String, Value<'a>>);

impl Metadata<'_> {
    fn text(&self, key: &str) -> String {
        self.0.get(key).map(text).unwrap_or_default()
    }

    fn int(&self, key: &str) -> Option<i64> {
        self.0.get(key).and_then(int)
    }
}

fn inner<'a>(v: &'a Value<'a>) -> &'a Value<'a> {
    match v {
        Value::Value(v) => inner(v),
        v => v,
    }
}

fn text(v: &Value) -> String {
    match inner(v) {
        Value::Str(s) => s.to_string(),
        Value::ObjectPath(p) => p.to_string(),
        Value::Array(a) => a.iter().map(text).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

fn int(v: &Value) -> Option<i64> {
    match *inner(v) {
        Value::I64(n) => Some(n),
        Value::U64(n) => i64::try_from(n).ok(),
        Value::I32(n) => Some(n.into()),
        Value::U32(n) => Some(n.into()),
        _ => None,
    }
}

fn is_ad(track_id: &str) -> bool {
    track_id.contains("/ad/") || track_id.starts_with("spotify:ad:")
}

fn cover(url: &str) -> Option<Cover> {
    let data = if let Some(path) = url.strip_prefix("file://") {
        std::fs::read(percent_decode(path)).ok()?
    } else if url.starts_with("https://") || url.starts_with("http://") {
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(3))).build().new_agent();
        agent.get(url).call().ok()?.body_mut().with_config().limit(MAX_COVER).read_to_vec().ok()?
    } else {
        return None;
    };
    let mime = match data.as_slice() {
        [0xFF, 0xD8, ..] => "image/jpeg",
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        _ => return None,
    };
    Some(Cover { mime, data })
}

fn percent_decode(s: &str) -> String {
    let (bytes, mut out, mut i) = (s.as_bytes(), Vec::with_capacity(s.len()), 0);
    while i < bytes.len() {
        match (bytes[i] == b'%').then(|| s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())).flatten() {
            Some(b) => (out.push(b), i += 3),
            None => (out.push(bytes[i]), i += 1),
        };
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Array;

    #[test]
    fn spotify_metadata_becomes_a_track() {
        let artists: Array = vec!["MRD", "Other"].into();
        let m = Metadata(HashMap::from([
            ("mpris:trackid".to_owned(), Value::from("/com/spotify/track/4uLU6hMCjMI75M1A2tKUQC")),
            ("mpris:length".to_owned(), Value::U64(215_986_000)),
            ("xesam:title".to_owned(), Value::from("Feel So Close")),
            ("xesam:artist".to_owned(), Value::Array(artists)),
        ]));
        assert_eq!(m.text("xesam:artist"), "MRD, Other");
        assert_eq!(m.int("mpris:length"), Some(215_986_000));
    }

    #[test]
    fn ads_are_recognised() {
        assert!(is_ad("/com/spotify/ad/123"));
        assert!(is_ad("spotify:ad:123"));
        assert!(!is_ad("/com/spotify/track/123"));
    }

    #[test]
    fn file_urls_are_decoded() {
        assert_eq!(percent_decode("/home/x/My%20Music/c%C3%B6ver.jpg"), "/home/x/My Music/cöver.jpg");
    }
}

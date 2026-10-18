//! Echolot API. Connecting is a PKCE sign-in: the approval page sends the browser back to
//! `mitschnitt://connected?code=…&state=…`, and code plus verifier buy a long-lived token (kept in the credential store).

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::runner::Job;

pub const REDIRECT: &str = "mitschnitt://connected";

pub fn app() -> String {
    format!("Mitschnitt {}", env!("CARGO_PKG_VERSION"))
}

pub fn platform() -> &'static str {
    std::env::consts::OS
}

pub fn pc_name() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_owned()))
        .unwrap_or_else(|| "Mitschnitt".into())
}

/// `https://Host/path/` -> `https://host/path`; a bare host gets `https://`.
pub fn normalize_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.contains("://") { url.to_owned() } else { format!("https://{url}") }
}

pub fn host(base: &str) -> &str {
    base.split("://").nth(1).unwrap_or(base).split('/').next().unwrap_or(base)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recorder {
    pub id: u64,
    pub name: String,
}

/// A recording session in Echolot, as every response that knows it reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: u64,
    #[serde(default)]
    pub title: String,
    /// `running`, `paused`, `done` or `stopped`.
    pub state: String,
    /// Why it is paused: `took_over`, `by_user` or `setup`.
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub total: u32,
    #[serde(default)]
    pub done: u32,
    #[serde(default)]
    pub filed: u32,
    #[serde(default)]
    pub not_better: u32,
    #[serde(default)]
    pub skipped: u32,
    #[serde(default)]
    pub failed: u32,
    #[serde(default)]
    pub minutes_left: u32,
    /// Length of all songs, and of those answered for (not the one being recorded).
    #[serde(default)]
    pub seconds_total: u64,
    #[serde(default)]
    pub seconds_done: u64,
}

impl SessionInfo {
    pub fn running(&self) -> bool {
        self.state == "running"
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub uri: String,
    pub artist: String,
    #[serde(default)]
    pub artists: Vec<String>,
    pub title: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub isrc: Option<String>,
    pub upload: String,
    pub result: String,
}

impl Item {
    pub fn job(&self) -> Job {
        Job {
            id: Some(self.id),
            uri: self.uri.clone(),
            artist: self.artist.clone(),
            artists: self.artists.clone(),
            title: self.title.clone(),
            album: self.album.clone(),
            duration_ms: self.duration_ms,
            isrc: self.isrc.clone().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Poll {
    pub item: Option<Item>,
    #[serde(default)]
    pub session: Option<SessionInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Me {
    pub recorder: Recorder,
    pub user: String,
    #[serde(default)]
    pub echolot: String,
    #[serde(default)]
    pub session: Option<SessionInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Connected {
    pub token: String,
    pub recorder: Recorder,
    pub user: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Filed {
    /// `filed`, `not_better` or `rejected`.
    pub result: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub session: Option<SessionInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceState {
    Idle,
    Busy,
    Recording,
    Setup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Spotify {
    Closed,
    Idle,
    Playing,
}

#[derive(Debug)]
pub enum Error {
    /// Disconnected in Echolot.
    Unauthorized,
    /// The item is no longer leased to this device.
    Conflict(String),
    NotFound(String),
    TooLarge,
    Status(u16, String),
    Network(String),
}

impl Error {
    pub fn transient(&self) -> bool {
        matches!(self, Error::Network(_)) || matches!(self, Error::Status(s, _) if *s >= 500)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unauthorized => write!(f, "Disconnected in Echolot"),
            Error::Conflict(d) => write!(f, "Echolot: not leased to this device ({d})"),
            Error::NotFound(d) => write!(f, "Echolot: {d}"),
            Error::TooLarge => write!(f, "Echolot: the file is too large"),
            Error::Status(s, d) => write!(f, "Echolot answered {s}: {d}"),
            Error::Network(e) => write!(f, "Echolot not reachable: {e}"),
        }
    }
}

type Response = ureq::http::Response<ureq::Body>;

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(timeout)).user_agent(app()).build().new_agent()
}

fn check(response: Result<Response, ureq::Error>) -> Result<Value, Error> {
    let mut response = response.map_err(|e| Error::Network(e.to_string()))?;
    let status = response.status().as_u16();
    let body: Value = response.body_mut().read_json().unwrap_or(Value::Null);
    let detail = || body["detail"].as_str().map(str::to_owned).unwrap_or_else(|| body.to_string());
    match status {
        200..=299 => Ok(body),
        401 => Err(Error::Unauthorized),
        404 => Err(Error::NotFound(detail())),
        409 => Err(Error::Conflict(detail())),
        413 => Err(Error::TooLarge),
        _ => Err(Error::Status(status, detail())),
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|e| Error::Status(200, format!("unexpected answer: {e}")))
}

#[derive(Clone)]
pub struct Client {
    pub base: String,
    token: String,
}

impl Client {
    pub fn new(base: &str, token: &str) -> Client {
        Client { base: normalize_url(base), token: token.to_owned() }
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http") { path.to_owned() } else { format!("{}{path}", self.base) }
    }

    fn post(&self, path: &str, body: &Value, timeout: Duration) -> Result<Value, Error> {
        let auth = format!("Bearer {}", self.token);
        check(agent(timeout).post(&self.url(path)).header("Authorization", &auth).send_json(body))
    }

    /// Heartbeat and request for work. Answers at once when there is work, otherwise after `wait` seconds.
    /// `fresh`: the first poll after Mitschnitt started; songs still leased to this device are given back.
    pub fn poll(&self, state: DeviceState, detail: &str, spotify: Spotify, item: Option<u64>, wait: u32, fresh: bool) -> Result<Poll, Error> {
        let mut body = json!({ "state": state, "detail": detail, "spotify": spotify, "app": app() });
        if let Some(id) = item {
            body["item"] = json!(id);
        }
        if fresh {
            body["fresh"] = json!(true);
        }
        parse(self.post(&format!("/api/recorder/poll?wait={wait}"), &body, Duration::from_secs(wait as u64 + 60))?)
    }

    pub fn upload(&self, item: &Item, file: &Path, note: Option<&str>) -> Result<Filed, Error> {
        let mut data = Vec::new();
        std::fs::File::open(file).and_then(|mut f| f.read_to_end(&mut data)).map_err(|e| Error::Network(format!("read {}: {e}", file.display())))?;
        let boundary = format!("mitschnitt{}", random_token(12));
        let name = file.file_name().map(|n| n.to_string_lossy().replace('"', "'")).unwrap_or_else(|| "recording.flac".into());
        let mut body = Vec::with_capacity(data.len() + 1024);
        if let Some(note) = note {
            body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\n{note}\r\n").as_bytes());
        }
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: audio/flac\r\n\r\n").as_bytes());
        body.extend_from_slice(&data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let auth = format!("Bearer {}", self.token);
        // Echolot decodes and checks the file before it answers: allow it a while.
        let request = agent(Duration::from_secs(600)).post(&self.url(&item.upload)).header("Authorization", &auth).header("Content-Type", &format!("multipart/form-data; boundary={boundary}"));
        parse(check(request.send(&body[..]))?)
    }

    pub fn result(&self, item: &Item, result: &str, detail: &str) -> Result<Option<SessionInfo>, Error> {
        let value = self.post(&item.result, &json!({ "result": result, "detail": detail }), Duration::from_secs(60))?;
        Ok(serde_json::from_value(value["session"].clone()).ok())
    }

    pub fn session(&self, action: &str) -> Result<Option<SessionInfo>, Error> {
        let value = self.post("/api/recorder/session", &json!({ "action": action }), Duration::from_secs(60))?;
        Ok(serde_json::from_value(value["session"].clone()).ok())
    }

    pub fn me(&self) -> Result<Me, Error> {
        let auth = format!("Bearer {}", self.token);
        parse(check(agent(Duration::from_secs(30)).get(&self.url("/api/recorder/me")).header("Authorization", &auth).call())?)
    }

    pub fn disconnect(&self) -> Result<(), Error> {
        let auth = format!("Bearer {}", self.token);
        check(agent(Duration::from_secs(30)).delete(&self.url("/api/recorder/me")).header("Authorization", &auth).call()).map(|_| ())
    }
}

#[derive(Debug, Clone)]
pub struct Pairing {
    pub base: String,
    pub verifier: String,
    pub state: String,
}

impl Pairing {
    pub fn new(base: &str) -> Pairing {
        Pairing { base: normalize_url(base), verifier: random_token(48), state: random_token(16) }
    }

    pub fn url(&self, name: &str) -> String {
        let q = |s: &str| encode(s);
        format!(
            "{}/recorders/connect?name={}&app={}&challenge={}&state={}&redirect={}",
            self.base,
            q(name),
            q(&app()),
            q(&challenge(&self.verifier)),
            q(&self.state),
            q(REDIRECT)
        )
    }

    pub fn claim(&self, code: &str, name: &str) -> Result<Connected, Error> {
        let body = json!({ "code": code.trim(), "verifier": self.verifier, "name": name, "app": app(), "platform": platform() });
        parse(check(agent(Duration::from_secs(30)).post(&format!("{}/api/recorder/connect", self.base)).send_json(&body))?)
    }
}

/// `base64url(sha256(verifier))` without padding.
pub fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("system random source");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(b) => {
                    out.push(b);
                    i += 3;
                    continue;
                }
                None => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    /// `mitschnitt://connect?url=<base>`: start connecting to that Echolot.
    Connect { url: String },
    /// `mitschnitt://connected?code=…&state=…`, or `error=cancelled`.
    Connected { code: Option<String>, state: String, error: Option<String> },
}

pub fn parse_link(link: &str) -> Option<Link> {
    let rest = link.trim().strip_prefix("mitschnitt://")?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let get = |key: &str| query.split('&').find_map(|p| p.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| decode(v)));
    match path.trim_end_matches('/') {
        "connect" => get("url").map(|url| Link::Connect { url }),
        "connected" => Some(Link::Connected { code: get("code"), state: get("state").unwrap_or_default(), error: get("error") }),
        _ => None,
    }
}

pub mod store {
    use std::path::PathBuf;

    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Saved {
        pub base: String,
        pub user: String,
        pub recorder: super::Recorder,
    }

    fn path() -> PathBuf {
        crate::settings::Settings::default_path().with_file_name("echolot.json")
    }

    pub fn load() -> Option<(Saved, String)> {
        let saved: Saved = serde_json::from_str(&std::fs::read_to_string(path()).ok()?).ok()?;
        let token = token::get(&saved.base)?;
        Some((saved, token))
    }

    pub fn save(saved: &Saved, token: &str) -> Result<(), String> {
        token::set(&saved.base, token)?;
        let path = path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(saved).unwrap_or_default()).map_err(|e| e.to_string())
    }

    pub fn forget() {
        if let Some((saved, _)) = load() {
            token::delete(&saved.base);
        }
        let _ = std::fs::remove_file(path());
    }

    #[cfg(windows)]
    mod token {
        fn entry(base: &str) -> Option<keyring::Entry> {
            keyring::Entry::new("Mitschnitt Echolot", base).ok()
        }

        pub fn get(base: &str) -> Option<String> {
            entry(base)?.get_password().ok()
        }

        pub fn set(base: &str, token: &str) -> Result<(), String> {
            entry(base).ok_or("credential store unavailable")?.set_password(token).map_err(|e| e.to_string())
        }

        pub fn delete(base: &str) {
            if let Some(e) = entry(base) {
                let _ = e.delete_credential();
            }
        }
    }

    /// Elsewhere (for now): a file only the user can read, next to the settings.
    #[cfg(not(windows))]
    mod token {
        use std::path::PathBuf;

        fn path() -> PathBuf {
            crate::settings::Settings::default_path().with_file_name("echolot.token")
        }

        pub fn get(_base: &str) -> Option<String> {
            std::fs::read_to_string(path()).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
        }

        pub fn set(_base: &str, token: &str) -> Result<(), String> {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path()).map_err(|e| e.to_string())?;
            f.write_all(token.as_bytes()).map_err(|e| e.to_string())
        }

        pub fn delete(_base: &str) {
            let _ = std::fs::remove_file(path());
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Export {
    #[serde(default)]
    pub title: String,
    pub tracks: Vec<ExportTrack>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportTrack {
    pub uri: String,
    pub artist: String,
    #[serde(default)]
    pub artists: Vec<String>,
    pub title: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub isrc: Option<String>,
}

impl Export {
    pub fn load(path: &Path) -> Result<Export, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Export, String> {
        serde_json::from_str(text).map_err(|e| format!("not an Echolot export ({e})"))
    }

    pub fn jobs(&self) -> Vec<Job> {
        self.tracks
            .iter()
            .filter(|t| t.uri.starts_with("spotify:track:"))
            .map(|t| Job {
                id: None,
                uri: t.uri.clone(),
                artist: t.artist.clone(),
                artists: t.artists.clone(),
                title: t.title.clone(),
                album: t.album.clone(),
                duration_ms: t.duration_ms,
                isrc: t.isrc.clone().unwrap_or_default(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let p = Pairing::new("echolot.example.org/");
        assert_eq!(p.base, "https://echolot.example.org");
        assert!((43..=128).contains(&p.verifier.len()));
        let url = p.url("DESKTOP-1");
        assert!(url.starts_with("https://echolot.example.org/recorders/connect?name=DESKTOP-1&app=Mitschnitt%20"));
        assert!(url.ends_with("&redirect=mitschnitt%3A%2F%2Fconnected"));
    }

    #[test]
    fn links_are_parsed() {
        assert_eq!(
            parse_link("mitschnitt://connected?code=ab%2Bc&state=xyz"),
            Some(Link::Connected { code: Some("ab+c".into()), state: "xyz".into(), error: None })
        );
        assert_eq!(parse_link("mitschnitt://connected/?error=cancelled&state=s"), Some(Link::Connected { code: None, state: "s".into(), error: Some("cancelled".into()) }));
        assert_eq!(parse_link("mitschnitt://connect?url=https%3A%2F%2Fecholot.example.org"), Some(Link::Connect { url: "https://echolot.example.org".into() }));
        assert_eq!(parse_link("https://example.org"), None);
    }

    #[test]
    fn items_and_exports_become_jobs() {
        let poll: Poll = serde_json::from_str(
            r#"{"item": {"id": 123, "key": "spotify:6fh", "uri": "spotify:track:6fh", "artist": "NYCO", "artists": ["NYCO"],
                "title": "Feel It", "album": "Feel It", "duration_ms": 225000, "isrc": null, "have": "lossy-low",
                "lease_until": "2026-10-12T14:20:00Z", "upload": "/api/recorder/items/123/file", "result": "/api/recorder/items/123/result"},
                "session": {"id": 7, "title": "Liked Songs", "state": "running", "reason": "", "total": 47, "done": 6, "filed": 5,
                "not_better": 0, "skipped": 1, "failed": 0, "minutes_left": 152}}"#,
        )
        .unwrap();
        let job = poll.item.unwrap().job();
        assert_eq!((job.id, job.track_id(), job.isrc.as_str()), (Some(123), "6fh", ""));
        assert!(poll.session.unwrap().running());
        let export: Export = serde_json::from_str(r#"{"title": "x", "tracks": [{"key": "k", "uri": "spotify:track:abc", "artist": "A", "title": "T", "duration_ms": 1000, "isrc": "I", "have": "missing"}]}"#).unwrap();
        assert_eq!(export.jobs()[0].isrc, "I");
    }

    #[test]
    fn urls() {
        assert_eq!(normalize_url(" https://echolot.example.org/ "), "https://echolot.example.org");
        assert_eq!(host("https://echolot.example.org"), "echolot.example.org");
    }
}

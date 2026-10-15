//! Every file Mitschnitt wrote, across runs: one JSON line per recording, and one more when an Echolot upload moved
//! (or deleted) it. Appending keeps writes small; [`load`] folds the lines together.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::analyze::{Lossy, Master, Severity};
use crate::passive::Track;
use crate::settings::Settings;

const LIMIT: usize = 1000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Unix ms.
    pub at: u64,
    pub track: Track,
    /// Where the file is now (a deleted file keeps its last place).
    pub file: String,
    pub size: u64,
    pub verdict: Severity,
    pub master: Master,
    pub exact: bool,
    /// Only the player's fade-in at the start is not original (older lines: see [`load`]).
    #[serde(default)]
    pub faded: bool,
    #[serde(default)]
    pub lossy: Option<Lossy>,
    pub echolot: bool,
    /// Echolot's answer to the upload: `filed`, `not_better`, `rejected` or `failed`.
    #[serde(default)]
    pub result: Option<String>,
    /// The check's findings, most important first (older lines have none).
    #[serde(default)]
    pub issues: Vec<String>,
    #[serde(default)]
    pub frames: Option<u64>,
    #[serde(default)]
    pub peak_dbfs: Option<f64>,
    #[serde(default, skip_deserializing)]
    pub exists: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Line {
    Record(Record),
    Moved { from: String, to: Option<String>, result: String },
}

pub fn default_path() -> PathBuf {
    Settings::default_path().with_file_name("history.jsonl")
}

static WRITE: Mutex<()> = Mutex::new(());

fn append(path: &Path, line: &Line) {
    let _guard = WRITE.lock();
    let Ok(mut json) = serde_json::to_string(line) else { return };
    json.push('\n');
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(json.as_bytes());
    }
}

pub fn add(path: &Path, record: &Record) {
    append(path, &Line::Record(record.clone()));
}

pub fn moved(path: &Path, from: &str, to: Option<&str>, result: &str) {
    append(path, &Line::Moved { from: from.into(), to: to.map(Into::into), result: result.into() });
}

pub fn load(path: &Path) -> Vec<Record> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut records: Vec<Record> = Vec::new();
    for line in text.lines().filter_map(|l| serde_json::from_str::<Line>(l).ok()) {
        match line {
            Line::Record(record) => records.push(record),
            Line::Moved { from, to, result } => {
                if let Some(r) = records.iter_mut().rev().find(|r| r.file == from) {
                    r.file = to.unwrap_or(from);
                    r.result = Some(result);
                }
            }
        }
    }
    records.reverse();
    records.truncate(LIMIT);
    for r in &mut records {
        r.exists = Path::new(&r.file).exists();
        // Lines from before `faded`: an altered recording Echolot answered was uploaded, and only a fade-in gets that far.
        r.faded |= r.verdict == Severity::Altered && r.echolot && r.result.is_some();
    }
    records
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upload_moves_the_recording_and_adds_echolots_answer() {
        let dir = std::env::temp_dir().join(format!("mitschnitt-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("history.jsonl");
        let kept = dir.join("kept.flac");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&kept, b"x").unwrap();
        let record = |title: &str, file: &str, echolot: bool| Record {
            at: 1,
            track: Track { title: title.into(), artist: "A".into(), album: String::new(), duration_ms: 1000 },
            file: file.into(),
            size: 1,
            verdict: Severity::Info,
            master: Master::Bit24,
            exact: true,
            faded: false,
            lossy: None,
            echolot,
            result: None,
            issues: Vec::new(),
            frames: None,
            peak_dbfs: None,
            exists: false,
        };
        add(&path, &record("Own", "own.flac", false));
        add(&path, &record("Uploaded", "pending.flac", true));
        add(&path, &record("Deleted", "pending2.flac", true));
        moved(&path, "pending.flac", Some(&kept.display().to_string()), "filed");
        moved(&path, "pending2.flac", None, "not_better");
        std::fs::write(&path, std::fs::read_to_string(&path).unwrap() + "not json\n").unwrap();

        let loaded = load(&path);
        let titles: Vec<_> = loaded.iter().map(|r| r.track.title.as_str()).collect();
        assert_eq!(titles, ["Deleted", "Uploaded", "Own"]);
        assert_eq!((loaded[1].result.as_deref(), loaded[1].exists), (Some("filed"), true));
        assert_eq!((loaded[0].result.as_deref(), loaded[0].file.as_str(), loaded[0].exists), (Some("not_better"), "pending2.flac", false));
        assert_eq!(loaded[2].result, None);

        // An older line without `faded`: altered and answered by Echolot means only faded in; not uploaded stays altered.
        let old = |file: &str| format!(r#"{{"at":2,"track":{{"title":"T","artist":"A","album":"","duration_ms":1}},"file":"{file}","size":1,"verdict":"altered","master":"bit16_partly","exact":false,"echolot":true}}"#);
        let moved = r#"{"from":"up.flac","to":null,"result":"filed"}"#;
        std::fs::write(&path, format!("{}\n{}\n{moved}\n", old("up.flac"), old("kept.flac"))).unwrap();
        let faded: Vec<_> = load(&path).iter().map(|r| (r.file.clone(), r.faded)).collect();
        assert_eq!(faded, [("kept.flac".to_string(), false), ("up.flac".to_string(), true)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

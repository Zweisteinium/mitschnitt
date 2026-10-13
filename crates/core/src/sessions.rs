//! Processes and app volumes. Apps render audio from child processes, so everything targets the app's whole
//! process tree.

use std::collections::HashSet;

pub use crate::platform::os::sessions::*;

#[derive(Debug, Clone)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
}

fn normalize(name: &str) -> String {
    name.to_lowercase().trim_end_matches(".exe").to_owned()
}

/// The topmost process whose executable matches `query` (e.g. "spotify" -> Spotify.exe).
pub fn find_app(query: &str, procs: &[Process]) -> Option<Process> {
    let q = normalize(query);
    let matches: Vec<&Process> = procs.iter().filter(|p| normalize(&p.name).contains(&q)).collect();
    let pids: HashSet<u32> = matches.iter().map(|p| p.pid).collect();
    matches.into_iter().filter(|p| !pids.contains(&p.parent)).min_by_key(|p| p.pid).cloned()
}

pub fn tree(root: u32, procs: &[Process]) -> HashSet<u32> {
    let mut set = HashSet::from([root]);
    loop {
        let before = set.len();
        set.extend(procs.iter().filter(|p| set.contains(&p.parent)).map(|p| p.pid).collect::<Vec<_>>());
        if set.len() == before {
            return set;
        }
    }
}

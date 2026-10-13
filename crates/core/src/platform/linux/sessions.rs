//! Processes from `/proc`, the app's PipeWire playback streams and their volume.

use std::collections::HashSet;

use super::devices::describe;
use super::pw::{Conn, Graph, Object};
use crate::devices::Endpoint;
use crate::sessions::{Process, find_app, tree};

/// A playback stream of the app (PipeWire node id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control(pub u32);

pub fn processes() -> Vec<Process> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).filter_map(process).collect()
}

/// From `/proc/<pid>/stat`: "pid (comm) state ppid ...", where comm may contain anything.
fn process(pid: u32) -> Option<Process> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (open, close) = (stat.find('(')?, stat.rfind(')')?);
    let parent = stat[close + 1..].split_whitespace().nth(1)?.parse().ok()?;
    Some(Process { pid, parent, name: stat[open + 1..close].to_owned() })
}

pub fn is_running(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[derive(Default)]
pub struct App {
    tree: HashSet<u32>,
    names: HashSet<String>,
}

impl App {
    pub fn new(tree: HashSet<u32>) -> App {
        let names = tree.iter().filter_map(|pid| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()).map(|s| s.trim().to_owned()).collect();
        App { tree, names }
    }

    pub fn of(pid: u32) -> App {
        App::new(tree(pid, &processes()))
    }

    /// Sandboxed apps (Flatpak) may report a pid of their own namespace, so the binary name counts as well.
    pub fn owns(&self, stream: &Object) -> bool {
        let pid = |key| stream.num(key).is_some_and(|pid| self.tree.contains(&pid));
        stream.class() == "Stream/Output/Audio"
            && (pid("application.process.id") || pid("client.pid") || stream.get("application.process.binary").is_some_and(|b| self.names.contains(&comm(b))))
    }

    pub fn streams<'g>(&self, graph: &'g Graph) -> impl Iterator<Item = &'g Object> {
        graph.nodes.iter().filter(|n| self.owns(n))
    }

    /// The output the app plays on (a running stream wins).
    pub fn sink<'g>(&self, graph: &'g Graph) -> Option<&'g Object> {
        let mut streams: Vec<&Object> = self.streams(graph).collect();
        streams.sort_by_key(|n| n.get("node.state") != Some("running"));
        streams.iter().flat_map(|s| graph.targets(s.id)).filter_map(|id| graph.node(id)).find(|n| n.class() == "Audio/Sink")
    }
}

/// A binary name as the kernel stores it (max. 15 bytes).
fn comm(binary: &str) -> String {
    let name = binary.rsplit('/').next().unwrap_or(binary);
    let mut end = name.len().min(15);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

pub fn app_volumes(tree: &HashSet<u32>) -> Vec<Control> {
    let Ok(conn) = Conn::new() else { return Vec::new() };
    App::new(tree.clone()).streams(&conn.graph()).map(|n| Control(n.id)).collect()
}

pub fn app_volumes_by_name(app: &str) -> Vec<Control> {
    let procs = processes();
    find_app(app, &procs).map(|root| app_volumes(&tree(root.pid, &procs))).unwrap_or_default()
}

/// Linux has no "Listen to this device" stream.
pub fn listen_volumes() -> Vec<Control> {
    Vec::new()
}

pub fn app_endpoint(tree: &HashSet<u32>) -> Option<Endpoint> {
    let conn = Conn::new().ok()?;
    let graph = conn.graph();
    App::new(tree.clone()).sink(&graph).map(|sink| describe(&conn, &graph, sink))
}

pub fn get(controls: &[Control]) -> Vec<f32> {
    let Ok(conn) = Conn::new() else { return Vec::new() };
    controls.iter().filter_map(|c| conn.volume(c.0).ok()).flat_map(|v| v.effective()).collect()
}

pub fn set(controls: &[Control], value: f32) {
    let Ok(conn) = Conn::new() else { return };
    for c in controls {
        let _ = conn.set_volume(c.0, value);
    }
}

pub fn muted(controls: &[Control]) -> Vec<bool> {
    let Ok(conn) = Conn::new() else { return Vec::new() };
    controls.iter().filter_map(|c| conn.volume(c.0).ok()).map(|v| v.mute).collect()
}

pub fn set_mute(_controls: &[Control], _mute: bool) {}

#[cfg(test)]
mod tests {
    use super::*;
    use pipewire::types::ObjectType;

    fn stream(class: &str, pid: &str, binary: &str) -> Object {
        let props = [("media.class", class), (if binary.is_empty() { "client.pid" } else { "application.process.id" }, pid), ("application.process.binary", binary)];
        Object { id: 1, type_: ObjectType::Node, props: props.into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect() }
    }

    #[test]
    fn streams_match_by_pid_or_binary() {
        let app = App { tree: HashSet::from([100, 101]), names: HashSet::from(["spotify".to_owned()]) };
        assert!(app.owns(&stream("Stream/Output/Audio", "101", "x")));
        assert!(app.owns(&stream("Stream/Output/Audio", "100", "")), "native PipeWire client");
        assert!(app.owns(&stream("Stream/Output/Audio", "2", "/app/extra/spotify")), "flatpak pid namespace");
        assert!(!app.owns(&stream("Stream/Input/Audio", "101", "spotify")));
        assert!(!app.owns(&stream("Stream/Output/Audio", "7", "firefox")));
    }

    #[test]
    fn comm_is_truncated_like_the_kernel() {
        assert_eq!(comm("/usr/lib/a-very-long-binary-name"), "a-very-long-bin");
    }

    #[test]
    fn own_process_is_found() {
        let me = std::process::id();
        assert!(is_running(me));
        assert!(processes().iter().any(|p| p.pid == me && p.parent > 0));
    }
}

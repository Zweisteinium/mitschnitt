//! Plays the app back through a PipeWire loopback: the app plays into its virtual sink, and its playback stream
//! goes to the default output. Its volume is that stream's mixer entry, synced both ways, while the app stays at
//! 100 %. The capture still takes the app's own ports, so the loopback never touches a recording.
//!
//! Like on Windows it stays on the output it started on ("home"): when the default output changes (headphones
//! off), it mutes instead of suddenly playing on the speakers, until home is back or the user unmutes.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;

use super::pw::Conn;
use super::sessions::App;
use crate::monitor::MonitorState;

pub const SUPPORTED: bool = true;

const SINK: &str = "mitschnitt_monitor";
const OUTPUT: &str = "mitschnitt_monitor.out";
const POLL: Duration = Duration::from_millis(250);

pub struct Monitor {
    /// The playback stream's level (linear, f32 bits), written by both sides. NaN until it is open.
    volume: Arc<AtomicU32>,
    state: Arc<MonitorState>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    /// `audible` off: the app still plays into the virtual sink, silently, as into the cable on Windows.
    pub fn start(_rate: u32, _channels: u16, _gain: f32, app_pid: u32, audible: bool) -> Option<Monitor> {
        let volume = Arc::new(AtomicU32::new(f32::NAN.to_bits()));
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(MonitorState::default());
        let (ready_tx, ready_rx) = mpsc::channel();
        let (v, s, st) = (volume.clone(), stop.clone(), state.clone());
        let thread = std::thread::Builder::new()
            .name("monitor".into())
            .spawn(move || {
                // Logged here: a monitor that ends during a run leaves the app on the speakers.
                if let Err(e) = run(app_pid, audible, &v, &st, &s, &ready_tx) {
                    crate::log!("monitor: {e}");
                    let _ = ready_tx.send(Err(e));
                }
            })
            .ok()?;
        match ready_rx.recv().unwrap_or_else(|e| Err(e.to_string())) {
            Ok(()) => Some(Monitor { volume, state, stop, thread: Some(thread) }),
            Err(_) => None,
        }
    }

    pub fn feed(&self, _data: &[f32], _frames: usize) {}

    pub fn volume_handle(&self) -> Arc<AtomicU32> {
        self.volume.clone()
    }

    pub fn state(&self) -> Arc<MonitorState> {
        self.state.clone()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(app_pid: u32, audible: bool, volume: &AtomicU32, state: &MonitorState, stop: &AtomicBool, ready: &mpsc::Sender<Result<(), String>>) -> Result<(), String> {
    let conn = Conn::new()?;
    let mut home = conn.graph().default_sink.ok_or("no default output device")?;
    let args = format!(
        r#"{{ node.description = "Mitschnitt monitor" audio.position = [ FL FR ]
            capture.props = {{ node.name = "{SINK}" media.class = "Audio/Sink" }}
            playback.props = {{ node.name = "{OUTPUT}" target.object = "{home}" }} }}"#
    );
    let module = conn.load_module("libpipewire-module-loopback", &args)?;
    conn.roundtrip();
    let _ = ready.send(Ok(()));
    crate::log!("monitor on \"{home}\"");

    let app = App::of(app_pid);
    // By serial: PipeWire gives a new stream a freed id again, and Spotify opens new ones now and then while it runs.
    let mut routed: Vec<String> = Vec::new();
    let mut applied: Option<(f32, bool)> = None;
    let mut next = Instant::now();
    let mut seen = conn.announced();
    while !stop.load(Ordering::Relaxed) {
        conn.mainloop.loop_().iterate(pw::loop_::Timeout::Finite(Duration::from_millis(50)));
        // At once for a new node: until it is routed, a new stream of the app plays on the speakers.
        if Instant::now() < next && conn.announced() == seen {
            continue;
        }
        seen = conn.announced();
        next = Instant::now() + POLL;
        let graph = conn.graph();
        let Some(out) = graph.nodes.iter().find(|n| n.get("node.name") == Some(OUTPUT)).map(|n| n.id) else { continue };
        // A stream the app opens later (on play) goes there too.
        let new: Vec<(u32, String)> = app.streams(&graph).filter_map(|s| Some((s.id, s.get("object.serial")?.to_owned()))).filter(|(_, serial)| !routed.contains(serial)).collect();
        for (id, serial) in new {
            conn.set_target(id, Some(SINK))?;
            routed.push(serial);
        }
        if state.rehome.swap(false, Ordering::Relaxed)
            && let Some(default) = &graph.default_sink
        {
            home.clone_from(default);
            conn.set_target(out, Some(&home))?;
            crate::log!("monitor on \"{home}\"");
        }
        let muted = graph.default_sink.as_deref() != Some(home.as_str());
        state.muted.store(muted, Ordering::Relaxed);
        if let Ok(level) = conn.volume(out).map(|v| v.channels.first().copied().unwrap_or(1.0)) {
            let ours = f32::from_bits(volume.load(Ordering::Relaxed));
            // Moved in the system's mixer: that is the level now.
            if ours.is_nan() || applied.is_some_and(|(a, _)| (a - level).abs() > 1e-4 && ours == a) {
                volume.store(level.to_bits(), Ordering::Relaxed);
                applied = applied.map(|(_, m)| (level, m));
            }
        }
        let want = (f32::from_bits(volume.load(Ordering::Relaxed)), muted || !audible);
        if applied != Some(want) {
            conn.set_volume_mute(out, want.0, want.1)?;
            applied = Some(want);
        }
    }
    for stream in app.streams(&conn.graph()).filter(|s| s.get("object.serial").is_some_and(|serial| routed.iter().any(|r| r == serial))) {
        let _ = conn.set_target(stream.id, None);
    }
    drop(module);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::process::Command;
    use std::rc::Rc;

    /// pacat plays silence three times in a row, like an app that opens a new stream after a pause; PipeWire hands
    /// the freed ids out again. Every stream must go into the monitor. Needs a running PipeWire with pipewire-pulse.
    #[test]
    #[ignore = "needs a running PipeWire session"]
    fn reopened_streams_go_into_the_monitor() {
        let wav = std::env::temp_dir().join(format!("mitschnitt-silence-{}.wav", std::process::id()));
        let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
        (0..48_000 * 2 * 2).for_each(|_| writer.write_sample(0i16).unwrap());
        writer.finalize().unwrap();

        let conn = Conn::new().unwrap();
        // (when, node or link id, added: node name or link's output and input node)
        type Seen = Rc<RefCell<Vec<(Instant, u32, Option<(String, String, String)>)>>>;
        let seen: Seen = Rc::default();
        let (added, removed) = (seen.clone(), seen.clone());
        let _listener = conn
            .registry
            .add_listener_local()
            .global(move |g| {
                let p = super::super::pw::dict(g.props);
                let get = |k: &str| p.get(k).cloned().unwrap_or_default();
                added.borrow_mut().push((Instant::now(), g.id, Some((get("node.name"), get("link.output.node"), get("link.input.node")))));
            })
            .global_remove(move |id| removed.borrow_mut().push((Instant::now(), id, None)))
            .register();

        let mut player = Command::new("sh").arg("-c").arg(format!("for i in 1 2 3; do pacat --file-format=wav '{}'; done", wav.display())).spawn().unwrap();
        // The app's processes are read once: pacat must be among them (not paplay: its streams say pacat).
        let tree = |pid| crate::sessions::tree(pid, &super::super::sessions::processes());
        while !tree(player.id()).iter().any(|p| std::fs::read_to_string(format!("/proc/{p}/comm")).is_ok_and(|c| c.trim() == "pacat")) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let monitor = Monitor::start(48_000, 2, 1.0, player.id(), false).expect("monitor");
        while player.try_wait().unwrap().is_none() {
            conn.mainloop.loop_().iterate(pw::loop_::Timeout::Finite(Duration::from_millis(10)));
        }
        conn.roundtrip();
        drop(monitor);
        let _ = std::fs::remove_file(&wav);

        let seen = seen.borrow();
        let mut names: HashMap<u32, String> = conn.objects().iter().map(|o| (o.id, o.get("node.name").unwrap_or_default().to_owned())).collect();
        #[derive(Default)]
        struct Stream {
            id: u32,
            output: Option<Instant>,
            unlinked: Option<Instant>,
            monitor: Option<Instant>,
        }
        let mut streams: Vec<Stream> = Vec::new();
        let mut links: HashMap<u32, (u32, bool)> = HashMap::new();
        for (at, id, what) in seen.iter() {
            match what {
                Some((name, out, _)) if out.is_empty() => {
                    names.insert(*id, name.clone());
                    if name == "pacat" {
                        streams.push(Stream { id: *id, ..Stream::default() });
                    }
                }
                Some((_, out, inp)) => {
                    let (out, inp): (u32, u32) = (out.parse().unwrap(), inp.parse().unwrap());
                    let Some(stream) = streams.iter_mut().rev().find(|s| s.id == out) else { continue };
                    let monitor = names.get(&inp).is_some_and(|n| n == SINK);
                    links.insert(*id, (out, monitor));
                    if monitor {
                        stream.monitor.get_or_insert(*at);
                    } else {
                        stream.output.get_or_insert(*at);
                    }
                }
                None => {
                    if let Some((out, false)) = links.remove(id)
                        && let Some(stream) = streams.iter_mut().rev().find(|s| s.id == out)
                    {
                        stream.unlinked = Some(*at);
                    }
                }
            }
        }
        // The first one played before the monitor started. Checked every 250 ms instead, the others played on the
        // output for about 60 ms each (measured on 2026-11-07); now not at all.
        assert_eq!(streams.len(), 3);
        for (i, s) in streams.iter().enumerate() {
            let on_output = s.output.map(|a| s.unlinked.map_or(Duration::MAX, |b| b.duration_since(a))).unwrap_or_default();
            eprintln!("stream {}: {on_output:?} on the output", s.id);
            assert!(s.monitor.is_some(), "stream {i} didn't go into the monitor");
            assert!(i == 0 || on_output < Duration::from_millis(30), "stream {i} played on the output for {on_output:?}");
        }
    }
}

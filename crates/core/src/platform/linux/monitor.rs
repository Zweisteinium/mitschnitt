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
                if let Err(e) = run(app_pid, audible, &v, &st, &s, &ready_tx) {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .ok()?;
        match ready_rx.recv().unwrap_or_else(|e| Err(e.to_string())) {
            Ok(()) => Some(Monitor { volume, state, stop, thread: Some(thread) }),
            Err(e) => {
                crate::log!("monitor: {e}");
                None
            }
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
    let mut routed: Vec<u32> = Vec::new();
    let mut applied: Option<(f32, bool)> = None;
    let mut next = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        conn.mainloop.loop_().iterate(pw::loop_::Timeout::Finite(Duration::from_millis(50)));
        if Instant::now() < next {
            continue;
        }
        next = Instant::now() + POLL;
        let graph = conn.graph();
        let Some(out) = graph.nodes.iter().find(|n| n.get("node.name") == Some(OUTPUT)).map(|n| n.id) else { continue };
        // A stream the app opens later (on play) goes there too; the session manager then remembers the target.
        let new: Vec<u32> = app.streams(&graph).map(|s| s.id).filter(|id| !routed.contains(id)).collect();
        for id in new {
            conn.set_target(id, Some(SINK))?;
            routed.push(id);
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
    for id in routed {
        let _ = conn.set_target(id, None);
    }
    drop(module);
    Ok(())
}

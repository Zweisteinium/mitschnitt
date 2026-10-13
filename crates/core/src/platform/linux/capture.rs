//! A capture stream of our own, linked by hand to the app's playback streams (after their volume, before any
//! mixing). Without autoconnect nothing else can be routed to us; streams created later get linked as they appear.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::types::ObjectType;

use super::pw::{Conn, Object, dict};
use super::sessions::App;
use crate::capture::{CaptureEvent, Packet};
use crate::passive::FLAG_SILENT;

/// PipeWire stops scheduling us while the app renders nothing (Windows keeps sending silent packets).
/// Longer pauses are reported as silent packets of the same length.
const GAP: Duration = Duration::from_millis(40);
const MAX_GAP: Duration = Duration::from_secs(10);

pub struct Capture {
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// `rate`: the rate of the app's output.
    pub fn start(pid: u32, rate: u32, channels: u16, sink: impl FnMut(CaptureEvent) + Send + 'static) -> Result<Capture, String> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (quit, quit_rx) = pw::channel::channel();
        let thread = std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                if let Err(e) = run(pid, rate, channels, sink, quit_rx, &ready_tx) {
                    let _ = ready_tx.send(Err(format!("could not open the capture: {e}")));
                }
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv().map_err(|e| e.to_string())??;
        Ok(Capture { quit, thread: Some(thread) })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    pid: u32,
    rate: u32,
    channels: u16,
    sink: impl FnMut(CaptureEvent) + 'static,
    quit: pw::channel::Receiver<()>,
    ready: &std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    let conn = Rc::new(Conn::new()?);
    let sink = Rc::new(RefCell::new(sink));
    let name = format!("mitschnitt-capture-{}", std::process::id());
    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::NODE_NAME => name.as_str(),
        *pw::keys::NODE_DESCRIPTION => "Mitschnitt",
        *pw::keys::APP_NAME => "Mitschnitt",
        "node.autoconnect" => "false",
        "stream.dont-remix" => "true",
    };
    let stream = pw::stream::StreamBox::new(&conn.core, "Mitschnitt", props).map_err(|e| e.to_string())?;

    let frame_bytes = 4 * channels as usize;
    let (errors, packets) = (sink.clone(), sink.clone());
    let mut last = (Instant::now(), 0);
    let _stream_listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_, _, _, state| {
            if let pw::stream::StreamState::Error(e) = state {
                (errors.borrow_mut())(CaptureEvent::Error(format!("capture stream: {e}")));
            }
        })
        .process(move |stream, _| {
            let emit = |packet| (packets.borrow_mut())(CaptureEvent::Packet(packet));
            while let Some(mut buffer) = stream.dequeue_buffer() {
                let Some(d) = buffer.datas_mut().first_mut() else { continue };
                let chunk = d.chunk();
                let (offset, frames) = (chunk.offset() as usize, chunk.size() as usize / frame_bytes);
                let empty = chunk.as_raw().flags & spa::sys::SPA_CHUNK_FLAG_EMPTY as i32 != 0;
                if frames == 0 {
                    continue;
                }
                let now = Instant::now();
                let gap = now.saturating_duration_since(last.0).saturating_sub(Duration::from_secs_f64(last.1 as f64 / rate as f64));
                if gap > GAP {
                    emit(Packet { data: Vec::new(), frames: (gap.min(MAX_GAP).as_secs_f64() * rate as f64) as usize, flags: FLAG_SILENT });
                }
                last = (now, frames);
                let bytes = d.data().filter(|_| !empty).and_then(|b| b.get(offset..offset + frames * frame_bytes));
                let data: Vec<f32> = bytes.map(|b| b.chunks_exact(4).map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]])).collect()).unwrap_or_default();
                let flags = if data.is_empty() { FLAG_SILENT } else { 0 };
                emit(Packet { data, frames, flags });
            }
        })
        .register()
        .map_err(|e| e.to_string())?;

    let format = format_pod(rate, channels)?;
    let mut params = [spa::pod::Pod::from_bytes(&format).ok_or("invalid format")?];
    stream.connect(spa::utils::Direction::Input, None, pw::stream::StreamFlags::MAP_BUFFERS, &mut params).map_err(|e| e.to_string())?;

    let links = Rc::new(RefCell::new(Links { app: App::of(pid), name, ..Links::default() }));
    let on_global = {
        let (links, conn) = (links.clone(), conn.clone());
        move |object: Object| match object.type_ {
            ObjectType::Node if object.is_stream() && object.class() == "Stream/Output/Audio" => {
                let Ok(node) = conn.bind::<pw::node::Node>(&object) else { return };
                let (l, c, id) = (links.clone(), conn.clone(), object.id);
                let listener = node
                    .add_listener_local()
                    .info(move |info| {
                        let mut links = l.borrow_mut();
                        links.stream(c.stream(id, dict(info.props())));
                        links.connect(&c.core);
                    })
                    .register();
                links.borrow_mut().watched.insert(id, (node, listener));
            }
            ObjectType::Node => links.borrow_mut().node(&object),
            ObjectType::Port => {
                let mut links = links.borrow_mut();
                links.port(&object);
                links.connect(&conn.core);
            }
            _ => {}
        }
    };
    let on_global = Rc::new(on_global);
    conn.objects().into_iter().for_each(|o| on_global(o));
    let (added, removed) = (on_global.clone(), links.clone());
    let _registry_listener = conn
        .registry
        .add_listener_local()
        .global(move |g| added(Object { id: g.id, type_: g.type_.clone(), props: dict(g.props) }))
        .global_remove(move |id| removed.borrow_mut().remove(id))
        .register();

    let mainloop = conn.mainloop.clone();
    let _quit = quit.attach(conn.mainloop.loop_(), move |_| mainloop.quit());
    let _ = ready.send(Ok(()));
    conn.mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}

fn format_pod(rate: u32, channels: u16) -> Result<Vec<u8>, String> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(rate);
    info.set_channels(channels.into());
    let mut position = [0; spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    match channels {
        1 => position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO,
        _ => position[..2].copy_from_slice(&[spa::sys::SPA_AUDIO_CHANNEL_FL, spa::sys::SPA_AUDIO_CHANNEL_FR]),
    }
    info.set_position(position);
    let object = spa::pod::Object { type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(), id: spa::param::ParamType::EnumFormat.as_raw(), properties: info.into() };
    let (cursor, _) = spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(object)).map_err(|e| format!("{e:?}"))?;
    Ok(cursor.into_inner())
}

/// App ports, our ports and the links between them (removed with their proxies).
#[derive(Default)]
struct Links {
    app: App,
    name: String,
    us: Option<u32>,
    inputs: HashMap<u32, String>,
    app_nodes: HashSet<u32>,
    outputs: HashMap<u32, (u32, String)>,
    watched: HashMap<u32, (pw::node::Node, pw::node::NodeListener)>,
    made: HashMap<(u32, u32), pw::link::Link>,
}

impl Links {
    fn node(&mut self, node: &Object) {
        if node.get("node.name") == Some(self.name.as_str()) {
            self.us = Some(node.id);
        }
    }

    fn stream(&mut self, node: Object) {
        if self.app.owns(&node) {
            self.app_nodes.insert(node.id);
        }
    }

    fn port(&mut self, port: &Object) {
        let (Some(node), Some(direction)) = (port.num("node.id"), port.get("port.direction")) else { return };
        let channel = port.get("audio.channel").unwrap_or("MONO").to_owned();
        match direction {
            "in" if Some(node) == self.us => _ = self.inputs.insert(port.id, channel),
            "out" if port.get("port.monitor") != Some("true") => _ = self.outputs.insert(port.id, (node, channel)),
            _ => {}
        }
    }

    fn remove(&mut self, id: u32) {
        self.app_nodes.remove(&id);
        self.watched.remove(&id);
        self.outputs.remove(&id);
        self.inputs.remove(&id);
        self.made.retain(|(out, inp), _| *out != id && *inp != id);
    }

    /// Link every app output port to our port of the same channel (a mono stream to all of ours).
    fn connect(&mut self, core: &pw::core::CoreRc) {
        let Some(us) = self.us else { return };
        for (&out, (node, channel)) in self.outputs.iter().filter(|(_, (node, _))| self.app_nodes.contains(node)) {
            for (&inp, ours) in &self.inputs {
                if (channel != ours && channel != "MONO") || self.made.contains_key(&(out, inp)) {
                    continue;
                }
                let props = properties! {
                    "link.output.node" => node.to_string(),
                    "link.output.port" => out.to_string(),
                    "link.input.node" => us.to_string(),
                    "link.input.port" => inp.to_string(),
                };
                if let Ok(link) = core.create_object::<pw::link::Link>("link-factory", &props) {
                    self.made.insert((out, inp), link);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    /// Plays 16-bit noise with `paplay` into a temporary null sink (silent) and captures it: every sample must
    /// arrive exactly, and the stream volume must be settable. Needs a running PipeWire with pipewire-pulse.
    #[test]
    #[ignore = "needs a running PipeWire session"]
    fn stream_is_captured_bit_exact() {
        let wav = std::env::temp_dir().join(format!("mitschnitt-noise-{}.wav", std::process::id()));
        let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
        let mut x: u32 = 1;
        let sent: Vec<i16> = (0..48_000 * 3 * 2)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 18) as i16 - 8192
            })
            .collect();
        sent.iter().for_each(|s| writer.write_sample(*s).unwrap());
        writer.finalize().unwrap();

        let sink = NullSink::load();
        let mut player = Command::new("paplay").arg(format!("--device={}", sink.name)).arg(&wav).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(300));

        // The session manager restores the last volume of `pacat` streams: start at 100 %.
        let controls = crate::sessions::app_volumes(&HashSet::from([player.id()]));
        crate::sessions::set(&controls, 0.5);
        assert_eq!(crate::sessions::get(&controls), [0.5, 0.5]);
        crate::sessions::set(&controls, 1.0);
        assert_eq!(crate::sessions::get(&controls), [1.0, 1.0]);
        std::thread::sleep(Duration::from_millis(100));

        let got = Arc::new(Mutex::new(Vec::<f32>::new()));
        let g = got.clone();
        let capture = Capture::start(player.id(), 48_000, 2, move |e| {
            if let CaptureEvent::Packet(p) = e {
                g.lock().unwrap().extend(p.data);
            }
        })
        .unwrap();
        let _ = player.wait();
        drop((capture, sink));
        let _ = std::fs::remove_file(&wav);

        let got = got.lock().unwrap();
        let scaled = |s: &i16| *s as f32 / 32768.0;
        let got = got.get(48_000..).expect("too little captured");
        let at = sent.windows(64).position(|w| w.iter().map(scaled).eq(got[..64].iter().copied())).expect("captured audio not found");
        let n = got.len().min(sent.len() - at);
        assert!(n > 48_000, "only {} frames compared", n / 2);
        assert!(got[..n].iter().zip(&sent[at..]).all(|(a, b)| *a == scaled(b)), "samples differ");
    }

    /// A silent output for the test, removed even when the test fails.
    struct NullSink {
        name: String,
        module: String,
    }

    impl NullSink {
        fn load() -> NullSink {
            let name = format!("mitschnitt-test-{}", std::process::id());
            let args = ["load-module", "module-null-sink", &format!("sink_name={name}"), "sink_properties=device.description=Mitschnitt-test"];
            let module = String::from_utf8_lossy(&Command::new("pactl").args(args).output().unwrap().stdout).trim().to_owned();
            NullSink { name, module }
        }
    }

    impl Drop for NullSink {
        fn drop(&mut self) {
            let _ = Command::new("pactl").args(["unload-module", &self.module]).status();
        }
    }
}

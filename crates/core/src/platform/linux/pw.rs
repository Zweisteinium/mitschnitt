//! PipeWire connection: graph snapshots and node volumes, request/response style on the calling thread.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::CString;
use std::rc::Rc;
use std::sync::Once;

use pipewire as pw;
use pw::spa;
use pw::types::ObjectType;
use spa::param::ParamType;
use spa::pod::serialize::PodSerializer;
use spa::pod::{Object as PodObject, Pod, Property, Value, ValueArray};
use spa::utils::Id;

#[derive(Debug, Clone)]
pub struct Object {
    pub id: u32,
    pub type_: ObjectType,
    pub props: HashMap<String, String>,
}

impl Object {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.props.get(key).map(String::as_str)
    }

    pub fn num(&self, key: &str) -> Option<u32> {
        self.get(key)?.parse().ok()
    }

    pub fn class(&self) -> &str {
        self.get("media.class").unwrap_or_default()
    }

    /// Streams name their app (`application.process.*`) only in their full info, which needs a bind.
    pub fn is_stream(&self) -> bool {
        self.type_ == ObjectType::Node && self.class().starts_with("Stream/")
    }
}

#[derive(Debug, Default)]
pub struct Graph {
    pub nodes: Vec<Object>,
    pub links: Vec<Object>,
    pub default_sink: Option<String>,
    pub clock_rate: Option<u32>,
    pub force_rate: Option<u32>,
}

impl Graph {
    pub fn node(&self, id: u32) -> Option<&Object> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn targets(&self, node: u32) -> impl Iterator<Item = u32> + '_ {
        self.links.iter().filter(move |l| l.num("link.output.node") == Some(node)).filter_map(|l| l.num("link.input.node"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Volume {
    pub volume: f32,
    pub channels: Vec<f32>,
    pub mute: bool,
}

impl Volume {
    pub fn effective(&self) -> Vec<f32> {
        let gain = if self.mute { 0.0 } else { self.volume };
        self.channels.iter().map(|c| c * gain).collect()
    }
}

pub struct Conn {
    pub mainloop: pw::main_loop::MainLoopRc,
    pub core: pw::core::CoreRc,
    pub registry: pw::registry::RegistryRc,
    /// Every global, kept because the server announces each one only once.
    objects: Rc<RefCell<Vec<Object>>>,
    _listener: pw::registry::Listener,
    _context: pw::context::ContextRc,
}

impl Conn {
    pub fn new() -> Result<Conn, String> {
        static INIT: Once = Once::new();
        INIT.call_once(pw::init);
        let err = |e: pw::Error| format!("PipeWire: {e}");
        let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(err)?;
        let context = pw::context::ContextRc::new(&mainloop, None).map_err(err)?;
        let core = context.connect_rc(None).map_err(|e| format!("PipeWire is not running ({e})"))?;
        let registry = core.get_registry_rc().map_err(err)?;
        let objects: Rc<RefCell<Vec<Object>>> = Rc::default();
        let (added, removed) = (objects.clone(), objects.clone());
        let listener = registry
            .add_listener_local()
            .global(move |g| added.borrow_mut().push(Object { id: g.id, type_: g.type_.clone(), props: dict(g.props) }))
            .global_remove(move |id| removed.borrow_mut().retain(|o| o.id != id))
            .register();
        let conn = Conn { mainloop, core, registry, objects, _listener: listener, _context: context };
        conn.roundtrip();
        Ok(conn)
    }

    /// Process events until the server has handled everything sent so far.
    pub fn roundtrip(&self) {
        let done = Rc::new(Cell::new(false));
        let Ok(pending) = self.core.sync(0) else { return };
        let (d, l) = (done.clone(), self.mainloop.clone());
        let _listener = self
            .core
            .add_listener_local()
            .done(move |id, seq| {
                if id == pw::core::PW_ID_CORE && seq == pending {
                    d.set(true);
                    l.quit();
                }
            })
            .register();
        while !done.get() {
            self.mainloop.run();
        }
    }

    pub fn objects(&self) -> Vec<Object> {
        self.objects.borrow().clone()
    }

    pub fn bind<T: pw::proxy::ProxyT>(&self, object: &Object) -> Result<T, String> {
        let global = pw::registry::GlobalObject {
            id: object.id,
            permissions: pw::permissions::PermissionFlags::empty(),
            type_: object.type_.clone(),
            version: 0,
            props: None::<pw::properties::PropertiesBox>,
        };
        self.registry.bind(&global).map_err(|e| e.to_string())
    }

    fn bind_node(&self, id: u32) -> Result<pw::node::Node, String> {
        let objects = self.objects.borrow();
        let node = objects.iter().find(|o| o.id == id && o.type_ == ObjectType::Node).ok_or_else(|| format!("audio node {id} is gone"))?;
        self.bind(node)
    }

    pub fn graph(&self) -> Graph {
        let graph = Rc::new(RefCell::new(Graph::default()));
        // Bound objects send their info and properties while they live.
        let mut proxies: Vec<Box<dyn std::any::Any>> = Vec::new();
        for object in self.objects() {
            match object.type_ {
                ObjectType::Node => {
                    if object.is_stream() && let Ok(node) = self.bind::<pw::node::Node>(&object) {
                        let (g, id) = (graph.clone(), object.id);
                        let listener = node
                            .add_listener_local()
                            .info(move |info| {
                                if let Some(n) = g.borrow_mut().nodes.iter_mut().find(|n| n.id == id) {
                                    n.props.extend(dict(info.props()));
                                }
                            })
                            .register();
                        proxies.push(Box::new((node, listener)));
                    }
                    graph.borrow_mut().nodes.push(object);
                }
                ObjectType::Link => graph.borrow_mut().links.push(object),
                ObjectType::Metadata if matches!(object.get("metadata.name"), Some("default" | "settings")) => {
                    let Ok(meta) = self.bind::<pw::metadata::Metadata>(&object) else { continue };
                    let g = graph.clone();
                    let listener = meta
                        .add_listener_local()
                        .property(move |subject, key, _type, value| {
                            if subject == 0 {
                                apply_setting(&mut g.borrow_mut(), key.unwrap_or_default(), value.unwrap_or_default());
                            }
                            0
                        })
                        .register();
                    proxies.push(Box::new((meta, listener)));
                }
                _ => {}
            }
        }
        self.roundtrip();
        drop(proxies);
        let mut graph = graph.take();
        for node in graph.nodes.iter_mut().filter(|n| n.is_stream()) {
            *node = self.stream(node.id, std::mem::take(&mut node.props));
        }
        graph
    }

    /// The current values of one param, as raw pods: the generic deserializer rejects `Props`
    /// (mixed-type `params` struct), so values are picked out with [`prop`].
    fn params(&self, id: u32, kind: ParamType) -> Result<Vec<Vec<u8>>, String> {
        let values: Rc<RefCell<Vec<Vec<u8>>>> = Rc::default();
        let v = values.clone();
        let node = self.bind_node(id)?;
        let _listener = node
            .add_listener_local()
            .param(move |_seq, _id, _index, _next, pod| v.borrow_mut().extend(pod.map(|p| p.as_bytes().to_vec())))
            .register();
        node.enum_params(0, Some(kind), 0, u32::MAX);
        self.roundtrip();
        Ok(values.take())
    }

    pub fn volume(&self, id: u32) -> Result<Volume, String> {
        let mut out = Volume { volume: 1.0, channels: Vec::new(), mute: false };
        for bytes in self.params(id, ParamType::Props)? {
            if let Some(v) = prop(&bytes, spa::sys::SPA_PROP_volume).and_then(|p| p.get_float().ok()) {
                out.volume = v;
            }
            if let Some(m) = prop(&bytes, spa::sys::SPA_PROP_mute).and_then(|p| p.get_bool().ok()) {
                out.mute = m;
            }
            if let Some(v) = prop(&bytes, spa::sys::SPA_PROP_channelVolumes).and_then(floats) {
                out.channels = v;
            }
        }
        if out.channels.is_empty() {
            return Err(format!("audio node {id} has no volume"));
        }
        Ok(out)
    }

    /// Set every channel of node `id` to `value` and unmute it.
    pub fn set_volume(&self, id: u32, value: f32) -> Result<(), String> {
        self.set_volume_mute(id, value, false)
    }

    pub fn set_volume_mute(&self, id: u32, value: f32, mute: bool) -> Result<(), String> {
        let channels = self.volume(id)?.channels.len();
        self.set_props(
            id,
            vec![
                Property::new(spa::sys::SPA_PROP_channelVolumes, Value::ValueArray(ValueArray::Float(vec![value; channels]))),
                Property::new(spa::sys::SPA_PROP_mute, Value::Bool(mute)),
            ],
        )
    }

    fn set_props(&self, id: u32, properties: Vec<Property>) -> Result<(), String> {
        let object = PodObject { type_: spa::utils::SpaTypes::ObjectParamProps.as_raw(), id: ParamType::Props.as_raw(), properties };
        let bytes = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(object)).map_err(|e| format!("{e:?}"))?.0.into_inner();
        let pod = Pod::from_bytes(&bytes).ok_or("invalid pod")?;
        self.bind_node(id)?.set_param(ParamType::Props, 0, pod);
        self.roundtrip();
        Ok(())
    }

    /// Write a key of the "settings" metadata (subject 0).
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        self.set_metadata("settings", 0, key, Some(value))
    }

    /// Where the session manager links stream `subject` (a node name), `None` to let it choose again.
    pub fn set_target(&self, subject: u32, target: Option<&str>) -> Result<(), String> {
        self.set_metadata("default", subject, "target.object", target)
    }

    fn set_metadata(&self, name: &str, subject: u32, key: &str, value: Option<&str>) -> Result<(), String> {
        let object = self.objects().into_iter().find(|o| o.type_ == ObjectType::Metadata && o.get("metadata.name") == Some(name));
        let metadata = self.bind::<pw::metadata::Metadata>(&object.ok_or_else(|| format!("PipeWire has no {name} metadata"))?)?;
        metadata.set_property(subject, key, None, value);
        self.roundtrip();
        Ok(())
    }

    /// A module of this process, such as a loopback; it goes when dropped or with the process.
    pub fn load_module(&self, name: &str, args: &str) -> Result<Module, String> {
        let (c_name, c_args) = (CString::new(name).map_err(|e| e.to_string())?, CString::new(args).map_err(|e| e.to_string())?);
        // SAFETY: the context outlives the call; PipeWire copies both strings.
        let module = unsafe { pw::sys::pw_context_load_module(self._context.as_raw_ptr(), c_name.as_ptr(), c_args.as_ptr(), std::ptr::null_mut()) };
        if module.is_null() {
            return Err(format!("PipeWire could not load {name}"));
        }
        Ok(Module(module))
    }

    /// A stream with its full info. Native PipeWire apps are known by their client's pid
    /// (PulseAudio apps all share pipewire-pulse's client and name themselves in the stream).
    pub fn stream(&self, id: u32, info: HashMap<String, String>) -> Object {
        let mut props = info;
        let client = props.get("client.id").and_then(|c| c.parse::<u32>().ok());
        let pid = client.and_then(|c| self.objects.borrow().iter().find(|o| o.id == c).and_then(|o| o.get("pipewire.sec.pid").map(str::to_owned)));
        props.extend(pid.map(|pid| ("client.pid".to_owned(), pid)));
        Object { id, type_: ObjectType::Node, props }
    }

    /// The rate node `id` runs at right now, if it runs.
    pub fn format_rate(&self, id: u32) -> Option<u32> {
        let params = self.params(id, ParamType::Format).ok()?;
        params.iter().find_map(|bytes| prop(bytes, spa::sys::SPA_FORMAT_AUDIO_rate)?.get_int().ok()).and_then(|r| u32::try_from(r).ok())
    }
}

/// Drop it before the [`Conn`] that loaded it.
pub struct Module(*mut pw::sys::pw_impl_module);

impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: loaded by `load_module` and destroyed only here.
        unsafe { pw::sys::pw_impl_module_destroy(self.0) }
    }
}

pub fn dict(props: Option<&spa::utils::dict::DictRef>) -> HashMap<String, String> {
    props.map(|p| p.iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect()).unwrap_or_default()
}

fn prop(bytes: &[u8], key: u32) -> Option<&Pod> {
    Pod::from_bytes(bytes)?.as_object().ok()?.find_prop(Id(key)).map(|p| p.value())
}

/// A float array pod: size, type, child size, child type, values.
fn floats(pod: &Pod) -> Option<Vec<f32>> {
    let b = pod.as_bytes();
    let word = |i: usize| Some(u32::from_ne_bytes(b.get(i..i + 4)?.try_into().ok()?));
    if !pod.is_array() || word(8)? != 4 || word(12)? != spa::sys::SPA_TYPE_Float {
        return None;
    }
    let end = (8 + word(0)? as usize).min(b.len());
    Some(b.get(16..end)?.chunks_exact(4).map(|w| f32::from_ne_bytes([w[0], w[1], w[2], w[3]])).collect())
}

fn apply_setting(g: &mut Graph, key: &str, value: &str) {
    let num = |s: &str| s.trim().parse::<u32>().ok();
    match key {
        "default.audio.sink" => g.default_sink = serde_json::from_str::<serde_json::Value>(value).ok().and_then(|v| Some(v.get("name")?.as_str()?.to_owned())),
        "clock.rate" => g.clock_rate = num(value),
        "clock.force-rate" => g.force_rate = num(value).filter(|r| *r > 0),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_metadata_is_parsed() {
        let mut g = Graph::default();
        apply_setting(&mut g, "default.audio.sink", r#"{ "name": "alsa_output.usb-dac" }"#);
        apply_setting(&mut g, "clock.force-rate", "0");
        assert_eq!(g.default_sink.as_deref(), Some("alsa_output.usb-dac"));
        assert_eq!(g.force_rate, None);
    }
}

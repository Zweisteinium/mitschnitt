use super::pw::{Conn, Graph, Object};
use crate::devices::Endpoint;

pub fn render_endpoints() -> Result<Vec<Endpoint>, String> {
    let conn = Conn::new()?;
    let graph = conn.graph();
    Ok(graph.nodes.iter().filter(|n| n.class() == "Audio/Sink").map(|n| describe(&conn, &graph, n)).collect())
}

pub fn default_endpoint() -> Result<Endpoint, String> {
    let conn = Conn::new()?;
    let graph = conn.graph();
    let name = graph.default_sink.as_deref().ok_or("no default output device")?;
    let sink = graph.nodes.iter().find(|n| n.class() == "Audio/Sink" && n.get("node.name") == Some(name)).ok_or("the default output device is gone")?;
    Ok(describe(&conn, &graph, sink))
}

/// Run the whole graph at `rate` (PipeWire's `clock.force-rate`), so the app reaches us unresampled.
/// Returns the setting to go back to.
pub fn hold_rate(rate: u32) -> Result<Option<u32>, String> {
    let conn = Conn::new()?;
    let previous = conn.graph().force_rate.unwrap_or(0);
    conn.set_setting("clock.force-rate", &rate.to_string())?;
    Ok(Some(previous))
}

pub fn release_rate(previous: u32) {
    if let Ok(conn) = Conn::new() {
        let _ = conn.set_setting("clock.force-rate", &previous.to_string());
    }
}

/// An idle device has no rate yet; it wakes at the graph's rate.
pub fn describe(conn: &Conn, graph: &Graph, sink: &Object) -> Endpoint {
    Endpoint {
        id: sink.get("node.name").unwrap_or_default().to_owned(),
        name: sink.get("node.description").or(sink.get("node.nick")).or(sink.get("node.name")).unwrap_or("?").to_owned(),
        sample_rate: conn.format_rate(sink.id).or(graph.force_rate).or(graph.clock_rate).unwrap_or(48_000),
        channels: sink.num("audio.channels").unwrap_or(2) as u16,
    }
}

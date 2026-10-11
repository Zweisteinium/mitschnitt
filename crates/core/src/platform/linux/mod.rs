//! Linux: PipeWire (a capture stream linked straight to the app's playback stream), MPRIS over D-Bus.

pub mod capture;
pub mod desktop;
pub mod devices;
pub mod guard;
pub mod media;
pub mod monitor;
pub mod power;
pub mod sessions;
pub mod setup;

mod pw;

/// App volume while listening: PipeWire has no limiter, so 100 % is exact (and a no-op).
pub const GAIN: f32 = 1.0;

pub fn init_thread() {}

/// The session bus, shared by everything in the process.
fn bus() -> Result<&'static zbus::blocking::Connection, String> {
    static BUS: std::sync::OnceLock<zbus::blocking::Connection> = std::sync::OnceLock::new();
    if let Some(bus) = BUS.get() {
        return Ok(bus);
    }
    let bus = zbus::blocking::Connection::session().map_err(|e| format!("D-Bus: {e}"))?;
    Ok(BUS.get_or_init(|| bus))
}

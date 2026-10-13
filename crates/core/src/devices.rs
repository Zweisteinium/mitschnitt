pub use crate::platform::os::devices::{default_endpoint, hold_rate, release_rate, render_endpoints};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

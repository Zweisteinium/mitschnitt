//! Windows: WASAPI process loopback, SMTC, VB-Cable with the built-in monitor, media key guard.

pub mod capture;
pub mod desktop;
pub mod devices;
pub mod guard;
pub mod keys;
pub mod media;
pub mod monitor;
pub mod own_session;
pub mod power;
pub mod sessions;
pub mod setup;

/// App volume while listening: Windows limits streams near 0 dBFS before the loopback tap, so recording at
/// 100 % would alter loud masters. At exactly 0.5 the player's own multiply is exact and fully undone.
pub const GAIN: f32 = 0.5;

/// Prepare the calling thread for the platform APIs (COM).
pub fn init_thread() {
    devices::com_init();
}

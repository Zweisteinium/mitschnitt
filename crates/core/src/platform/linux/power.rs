use std::collections::HashMap;

use zbus::zvariant::{OwnedObjectPath, Value};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const SUSPEND: u32 = 4;

/// Keeps the computer from suspending (not the display) until dropped, via the desktop portal.
pub struct KeepAwake(Option<OwnedObjectPath>);

impl KeepAwake {
    pub fn start() -> KeepAwake {
        let request = super::bus().ok().and_then(|bus| {
            let options = HashMap::from([("reason", Value::from("Recording"))]);
            let reply = bus.call_method(Some(PORTAL), "/org/freedesktop/portal/desktop", Some("org.freedesktop.portal.Inhibit"), "Inhibit", &("", SUSPEND, options));
            reply.ok()?.body().deserialize().ok()
        });
        KeepAwake(request)
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        if let (Some(request), Ok(bus)) = (self.0.take(), super::bus()) {
            let _ = bus.call_method(Some(PORTAL), request.as_str(), Some("org.freedesktop.portal.Request"), "Close", &());
        }
    }
}

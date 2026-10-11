//! Protects a recording from media commands: a keyboard hook swallows media keys, and while a track records,
//! Mitschnitt's own media session receives headset and Bluetooth commands instead of the player.

use super::keys::KeyGuard;
use super::own_session::OwnSession;
use crate::media::Extra;
use crate::passive::Track;

pub const SUPPORTED: bool = true;

pub struct Guard {
    session: Option<OwnSession>,
    keys: KeyGuard,
    caught: u32,
}

impl Guard {
    pub fn start() -> Result<Guard, String> {
        Ok(Guard { session: None, keys: KeyGuard::start()?, caught: 0 })
    }

    /// Arm while `recording`; the session shows `current` (the track being recorded).
    pub fn update(&mut self, recording: bool, current: Option<&(Track, Extra)>) {
        self.keys.arm(recording);
        match (&self.session, recording) {
            (None, true) => {
                let (title, artist, cover) = match current {
                    Some((t, e)) => (format!("rec. {}", t.title), t.artist.as_str(), e.cover.as_ref().map(|c| c.data.as_slice())),
                    None => ("rec.".to_owned(), "", None),
                };
                self.session = OwnSession::start(&title, artist, cover).ok();
            }
            (Some(_), false) => self.session = None,
            _ => {}
        }
        if let Some(session) = &self.session {
            session.pump();
            self.caught += session.take_pressed().len() as u32;
        }
    }

    /// Commands swallowed so far.
    pub fn blocked(&self) -> u32 {
        self.keys.blocked() + self.caught
    }
}

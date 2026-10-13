//! Protects a recording from media commands: a keyboard hook swallows media keys, and our own media session
//! receives headset and Bluetooth commands instead of the player.
//!
//! Windows hands the current session back to the player behind our back (unlocking the PC does), so it is
//! re-registered after an unlock and whenever another app became current.

use std::time::{Duration, Instant};

use super::keys::{self, KeyGuard};
use super::media;
use super::own_session::OwnSession;
use crate::media::Extra;
use crate::passive::Track;

pub const SUPPORTED: bool = true;

const CHECK_EVERY: Duration = Duration::from_secs(1);

pub struct Guard {
    session: Option<OwnSession>,
    keys: KeyGuard,
    caught: u32,
    /// The executable's file name, as Windows names our session.
    own_id: String,
    unlocks: u32,
    checked: Instant,
}

impl Guard {
    pub fn start() -> Result<Guard, String> {
        let own_id = std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
        Ok(Guard { session: None, keys: KeyGuard::start()?, caught: 0, own_id, unlocks: keys::unlocks(), checked: Instant::now() })
    }

    /// The session shows `current`, the track being recorded.
    pub fn update(&mut self, recording: bool, current: Option<&(Track, Extra)>) {
        self.keys.arm(recording);
        if recording && self.session.is_some() && self.lost() {
            self.session = None; // re-registered below, which makes it current again
        }
        match (&self.session, recording) {
            (None, true) => {
                let (title, artist, cover) = match current {
                    Some((t, e)) => (format!("Rec. {}", t.title), t.artist.as_str(), e.cover.as_ref().map(|c| c.data.as_slice())),
                    None => ("Rec.".to_owned(), "", None),
                };
                self.session = OwnSession::start(&title, artist, cover).ok();
                self.unlocks = keys::unlocks();
                self.checked = Instant::now();
            }
            (Some(_), false) => self.session = None,
            _ => {}
        }
        if let Some(session) = &self.session {
            session.pump();
            self.caught += session.take_pressed();
        }
    }

    fn lost(&mut self) -> bool {
        let unlocks = keys::unlocks();
        if unlocks != self.unlocks {
            self.unlocks = unlocks;
            return true;
        }
        if self.checked.elapsed() < CHECK_EVERY {
            return false;
        }
        self.checked = Instant::now();
        // No current session at all (or an error) is left alone.
        media::manager()
            .and_then(|m| m.GetCurrentSession())
            .and_then(|s| s.SourceAppUserModelId())
            .is_ok_and(|id| !self.own_id.is_empty() && !id.to_string().eq_ignore_ascii_case(&self.own_id))
    }

    pub fn blocked(&self) -> u32 {
        self.keys.blocked() + self.caught
    }
}

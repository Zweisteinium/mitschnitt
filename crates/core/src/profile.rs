//! Supported apps. Everything app-specific lives here (how to find it, what it delivers, how to tell an ad);
//! the capture core is the same for all of them. An app gets a profile only after it was verified to deliver
//! untouched lossless audio (the 24-bit grid check of `passive`).

use serde::Serialize;

use crate::passive::Track;

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    /// Stable key, e.g. "spotify".
    pub id: &'static str,
    pub name: &'static str,
    /// Matched against executable names (".exe" ignored, case-insensitive).
    pub exe: &'static str,
    /// Matched against the media session's app id (case-insensitive substring).
    pub media_id: &'static str,
    /// The rate the app delivers; the cable must run at it, or the audio gets resampled.
    pub rate: u32,
    /// The app delivers lossless audio: every sample must pass the 24-bit grid check.
    pub lossless: bool,
    /// How to point the app's output at the cable by hand.
    pub output_hint: &'static str,
    #[serde(skip)]
    pub is_ad: fn(&Track) -> bool,
}

pub const SPOTIFY: Profile = Profile {
    id: "spotify",
    name: "Spotify",
    exe: "spotify",
    media_id: "spotify",
    rate: 44_100,
    lossless: true,
    output_hint: "In Spotify: Settings → Playback → Output device → CABLE Input.",
    is_ad: spotify_ad,
};

pub const PROFILES: &[Profile] = &[SPOTIFY];

pub fn get(id: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.id.eq_ignore_ascii_case(id))
}

impl Profile {
    /// A profile for any app, for experiments (CLI only): no setup checks, no lossless expectation.
    pub fn adhoc(app: &str) -> Profile {
        let app: &'static str = Box::leak(app.trim_end_matches(".exe").to_lowercase().into_boxed_str());
        Profile { id: app, name: app, exe: app, media_id: app, rate: 0, lossless: false, output_hint: "", is_ad: |_| false }
    }

    pub fn matches_exe(&self, exe: &str) -> bool {
        exe.to_lowercase().trim_end_matches(".exe").contains(self.exe)
    }

    pub fn matches_media(&self, app_id: &str) -> bool {
        app_id.to_lowercase().contains(self.media_id)
    }
}

/// Spotify Free reports ads like tracks: no artist, or "Spotify"/"Advertisement" as title or artist.
fn spotify_ad(t: &Track) -> bool {
    let generic = |s: &str| ["", "spotify", "advertisement", "werbung", "spotify free"].contains(&s.trim().to_lowercase().as_str());
    t.artist.trim().is_empty() || t.title.trim().eq_ignore_ascii_case("advertisement") || (generic(&t.artist) && t.album.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(title: &str, artist: &str, album: &str) -> Track {
        Track { title: title.into(), artist: artist.into(), album: album.into(), duration_ms: 30_000 }
    }

    #[test]
    fn spotify_ads_are_recognised() {
        let ad = SPOTIFY.is_ad;
        assert!(ad(&t("Advertisement", "", "")));
        assert!(ad(&t("Spotify", "Spotify", "")));
        assert!(ad(&t("Try Premium", "", "")));
        assert!(!ad(&t("Feel So Close", "MRD", "Feel So Close")));
        assert!(!ad(&t("Spotify", "Some Band", "Album")), "a song may be called Spotify");
    }

    #[test]
    fn matching_ignores_case_and_exe() {
        assert!(SPOTIFY.matches_exe("Spotify.exe"));
        assert!(SPOTIFY.matches_media("Spotify.exe"));
        assert!(!SPOTIFY.matches_media("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"));
        assert_eq!(get("SPOTIFY").map(|p| p.name), Some("Spotify"));
    }
}

//! Setup assistant: health checks of everything that affects a bit-exact recording, and one-click fixes
//! where they can be applied reliably (installing VB-Cable, its sample rate, routing the app to it).
//! Everything else is reported with a short how-to.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::ptr::addr_of;

use serde::Serialize;
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::Media::Audio::{EDataFlow, ERole, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, eConsole, eMultimedia, eRender};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, PCWSTR, w};
use windows_core::interface;

use crate::devices::{Endpoint, com_init, render_endpoints};
use crate::profile::Profile;
use crate::sessions;

const CABLE: &str = "CABLE Input";
const CABLE_ZIP: &str = "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ok,
    Warn,
    Bad,
    /// Can't be detected - the user has to check it.
    Check,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub title: &'static str,
    pub state: State,
    pub detail: String,
    /// One-click fix, see [`fix`].
    pub fix: Option<&'static str>,
}

fn check(id: &'static str, title: &'static str, state: State, detail: impl Into<String>, fix: Option<&'static str>) -> Check {
    Check { id, title, state, detail: detail.into(), fix }
}

/// All checks for recording with `profile`.
pub fn health(profile: &Profile) -> Vec<Check> {
    com_init();
    let mut out = Vec::new();
    let endpoints = render_endpoints().unwrap_or_default();
    let cable = endpoints.iter().find(|e| e.name.starts_with(CABLE));

    out.push(match cable {
        Some(_) => check("cable", "Virtual cable", State::Ok, "VB-Cable is installed", None),
        None => check(
            "cable",
            "Virtual cable",
            State::Bad,
            "VB-Cable (donationware by VB-Audio) gives the app a silent, effect-free output. Mitschnitt downloads the official package from vb-audio.com and starts its installer.",
            Some("install_cable"),
        ),
    });
    if let Some(cable) = cable {
        let khz = profile.rate as f64 / 1000.0;
        out.push(if cable.sample_rate == profile.rate {
            check("cable_rate", "Cable sample rate", State::Ok, format!("{khz} kHz - {}'s rate, no resampling", profile.name), None)
        } else {
            check("cable_rate", "Cable sample rate", State::Bad, format!("{} Hz - {} ({khz} kHz) would be resampled", cable.sample_rate, profile.name), Some("cable_rate"))
        });
    }

    let procs = sessions::processes();
    if let Some(root) = sessions::find_app(profile.exe, &procs) {
        let endpoint = sessions::app_endpoint(&sessions::tree(root.pid, &procs));
        out.push(match endpoint {
            Some(Endpoint { name, .. }) if name.starts_with(CABLE) => check("route", "App output", State::Ok, format!("{} plays into the cable", profile.name), None),
            Some(Endpoint { name, .. }) => check(
                "route",
                "App output",
                State::Bad,
                format!("{} plays on \"{name}\" (effects and resampling alter it). Route it to the cable - or by hand: {}", profile.name, profile.output_hint),
                cable.map(|_| "route_cable"),
            ),
            None => check("route", "App output", State::Check, format!("{} has no audio stream yet - play something once", profile.name), None),
        });
    } else {
        out.push(check("route", "App output", State::Check, format!("{} is not running", profile.name), None));
    }

    if profile.id == "spotify" {
        out.extend(spotify_checks());
    }
    if !sessions::listen_volumes().is_empty() {
        out.push(check("listen", "\"Listen to this device\"", State::Ok, "Active - Mitschnitt mutes it while listening and monitors itself", None));
    }
    out
}

/// What Spotify's settings file reveals (read-only; keys are undocumented and may move between versions).
fn spotify_checks() -> Vec<Check> {
    let mut out = Vec::new();
    let prefs = spotify_prefs().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let value = |key: &str| prefs.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('='))).map(str::to_owned);

    match value("audio.play_bitrate_enumeration").and_then(|v| v.parse::<u32>().ok()) {
        Some(q) if q >= 5 => out.push(check("quality", "Streaming quality", State::Ok, "Lossless", None)),
        Some(_) => out.push(check("quality", "Streaming quality", State::Bad, "Not lossless. In Spotify: Settings → Audio quality → Streaming quality → Lossless.", None)),
        None => out.push(check("quality", "Streaming quality", State::Check, "Set Spotify → Settings → Audio quality → Streaming quality → Lossless.", None)),
    }
    out.push(match value("audio.allow_downgrade").as_deref() {
        Some("false") => check("downgrade", "Auto-adjust quality", State::Ok, "Off", None),
        _ => check("downgrade", "Auto-adjust quality", State::Warn, "Turn off \"Auto adjust quality\" so a slow network can't drop to lossy.", None),
    });
    out.push(match value("audio.normalize_v2").as_deref() {
        Some("false") => check("normalize", "Normalize volume", State::Ok, "Off", None),
        _ => check("normalize", "Normalize volume", State::Bad, "Turn off \"Normalize volume\" - it changes the level of every track.", None),
    });
    if let Some(device) = value("audio.device_selected").and_then(|v| serde_json::from_str::<String>(&v).ok()) {
        let exclusive = serde_json::from_str::<serde_json::Value>(&device).ok().and_then(|d| d["excl"].as_bool()).unwrap_or(false);
        out.push(if exclusive {
            check("exclusive", "Exclusive mode", State::Bad, "On - Spotify bypasses the Windows mixer, nothing can be captured. Turn it off.", None)
        } else {
            check("exclusive", "Exclusive mode", State::Ok, "Off", None)
        });
    }
    out.push(check(
        "crossfade",
        "Crossfade, Automix, Equalizer",
        State::Check,
        "Can't be read from Spotify - make sure Crossfade, Automix and the Equalizer are off. Mitschnitt also detects altered audio in every file.",
        None,
    ));
    out
}

/// The most recently used `prefs` of the Spotify desktop app (classic and Microsoft Store install).
fn spotify_prefs() -> Option<PathBuf> {
    let roots = [
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Spotify").join("Users")),
        std::env::var_os("LOCALAPPDATA").map(|a| PathBuf::from(a).join(r"Packages\SpotifyAB.SpotifyMusic_zpdnekdrzrea0\LocalState\Spotify\Users")),
    ];
    roots
        .into_iter()
        .flatten()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flatten()
        .flatten()
        .map(|e| e.path().join("prefs"))
        .filter(|p| p.is_file())
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
}

/// Apply a one-click fix from [`Check::fix`]. Returns a message for the user.
pub fn fix(action: &str, profile: &Profile) -> Result<String, String> {
    com_init();
    match action {
        "install_cable" => install_cable(profile.rate),
        "cable_rate" => set_cable_rate(profile.rate),
        "route_cable" => route_to_cable(profile),
        other => Err(format!("unknown fix \"{other}\"")),
    }
}

/// Point the app at the cable the way Windows' "App volume and device preferences" page does: a per-app
/// default device that Windows remembers. The app must use its "default" output for this to apply.
fn route_to_cable(profile: &Profile) -> Result<String, String> {
    let cable = render_endpoints().map_err(|e| e.to_string())?.into_iter().find(|e| e.name.starts_with(CABLE)).ok_or("VB-Cable not found")?;
    let procs = sessions::processes();
    let root = sessions::find_app(profile.exe, &procs).ok_or_else(|| format!("{} is not running", profile.name))?;
    let device = HSTRING::from(format!(r"\\?\SWD#MMDEVAPI#{}#{{e6327cad-dcec-4949-ae8a-991e976a79d2}}", cable.id));
    // SAFETY: activation of a system WinRT factory; the device string outlives every call.
    unsafe {
        let factory: route::IAudioPolicyConfigFactory = windows::Win32::System::WinRT::RoGetActivationFactory(&HSTRING::from("Windows.Media.Internal.AudioPolicyConfig"))
            .map_err(|e| format!("Windows doesn't offer per-app routing here ({e}). {}", profile.output_hint))?;
        for pid in sessions::tree(root.pid, &procs) {
            for role in [eMultimedia, eConsole] {
                route::set_persisted(&factory, pid, eRender, role, &device).ok().map_err(|e| format!("routing failed: {e}"))?;
            }
        }
    }
    Ok(format!("{} now plays into the cable. If it still uses another device, set its own output to \"default\" or: {}", profile.name, profile.output_hint))
}

/// Undocumented WinRT factory behind Windows' per-app device page (Windows 10 21H2+ layout).
/// Only the vtable order matters; the unused methods are placeholders.
#[allow(non_snake_case)]
mod route {
    use super::*;

    /// An IInspectable; its three methods (GetIids, GetRuntimeClassName, GetTrustLevel) are placeholders too.
    #[interface("ab3d4648-e242-459f-b02f-541c70306324")]
    pub unsafe trait IAudioPolicyConfigFactory: IUnknown {
        fn _i0(&self) -> HRESULT;
        fn _i1(&self) -> HRESULT;
        fn _i2(&self) -> HRESULT;
        fn _0(&self) -> HRESULT;
        fn _1(&self) -> HRESULT;
        fn _2(&self) -> HRESULT;
        fn _3(&self) -> HRESULT;
        fn _4(&self) -> HRESULT;
        fn _5(&self) -> HRESULT;
        fn _6(&self) -> HRESULT;
        fn _7(&self) -> HRESULT;
        fn _8(&self) -> HRESULT;
        fn _9(&self) -> HRESULT;
        fn _10(&self) -> HRESULT;
        fn _11(&self) -> HRESULT;
        fn _12(&self) -> HRESULT;
        fn _13(&self) -> HRESULT;
        fn _14(&self) -> HRESULT;
        fn _15(&self) -> HRESULT;
        fn _16(&self) -> HRESULT;
        fn _17(&self) -> HRESULT;
        fn _18(&self) -> HRESULT;
        fn SetPersistedDefaultAudioEndpoint(&self, pid: u32, flow: EDataFlow, role: ERole, device: *mut std::ffi::c_void) -> HRESULT;
    }

    /// SAFETY: `factory` must be the real activation factory.
    pub unsafe fn set_persisted(factory: &IAudioPolicyConfigFactory, pid: u32, flow: EDataFlow, role: ERole, device: &HSTRING) -> HRESULT {
        // An HSTRING is a single handle; the callee doesn't take ownership.
        unsafe { factory.SetPersistedDefaultAudioEndpoint(pid, flow, role, std::mem::transmute_copy(device)) }
    }
}

/// Download the official VB-Cable package and run VB-Audio's installer elevated (Windows asks for consent
/// and shows its driver prompt). Nothing of VB-Audio is shipped with Mitschnitt.
fn install_cable(rate: u32) -> Result<String, String> {
    let zip = ureq::get(CABLE_ZIP)
        .call()
        .map_err(|e| format!("download: {e}"))?
        .body_mut()
        .with_config()
        .limit(32 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("download: {e}"))?;
    let dir = std::env::temp_dir().join("Mitschnitt-VBCABLE");
    let _ = std::fs::remove_dir_all(&dir);
    zip::ZipArchive::new(Cursor::new(zip)).and_then(|mut a| a.extract(&dir)).map_err(|e| format!("unpack: {e}"))?;
    let setup = dir.join("VBCABLE_Setup_x64.exe");
    if !setup.is_file() {
        return Err("the package has no VBCABLE_Setup_x64.exe".into());
    }
    let code = run_elevated(&setup, "-i -h")?;
    let _ = std::fs::remove_dir_all(&dir);
    if render_endpoints().unwrap_or_default().iter().any(|e| e.name.starts_with(CABLE)) {
        set_cable_rate(rate).map(|m| format!("VB-Cable installed. {m}"))
    } else {
        Err(format!("the installer finished (code {code}) but no cable is visible yet - a restart of Windows may be needed"))
    }
}

fn run_elevated(exe: &Path, args: &str) -> Result<u32, String> {
    let (file, params) = (HSTRING::from(exe.as_os_str()), HSTRING::from(args));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: HWND::default(),
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: `info` and the strings it points to outlive the call; the process handle is closed here.
    unsafe {
        ShellExecuteExW(&mut info).map_err(|e| format!("could not start the installer: {e}"))?;
        WaitForSingleObject(info.hProcess, 10 * 60 * 1000);
        let mut code = 0u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        Ok(code)
    }
}

/// Undocumented but long-stable endpoint configuration interface (used by the Sound control panel).
/// Method names keep their COM spelling; only the vtable order matters.
#[allow(non_snake_case)]
mod policy {
    use super::*;

    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    pub unsafe trait IPolicyConfig: IUnknown {
        fn GetMixFormat(&self, device: PCWSTR, format: *mut *mut WAVEFORMATEX) -> HRESULT;
        fn GetDeviceFormat(&self, device: PCWSTR, default: i32, format: *mut *mut WAVEFORMATEX) -> HRESULT;
        fn ResetDeviceFormat(&self, device: PCWSTR) -> HRESULT;
        fn SetDeviceFormat(&self, device: PCWSTR, endpoint: *const WAVEFORMATEX, mix: *const WAVEFORMATEX) -> HRESULT;
    }

    /// SAFETY: `endpoint` and `mix` must point to valid formats for the duration of the call.
    pub unsafe fn set_device_format(policy: &IPolicyConfig, device: PCWSTR, endpoint: *const WAVEFORMATEX, mix: *const WAVEFORMATEX) -> HRESULT {
        unsafe { policy.SetDeviceFormat(device, endpoint, mix) }
    }
}
use policy::{IPolicyConfig, set_device_format};

/// `KSDATAFORMAT_SUBTYPE_PCM` (defined here to avoid the large KernelStreaming feature).
const KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// Set the cable to 2 ch / 24-bit / `rate` (engine mix: 32-bit float at `rate`).
fn set_cable_rate(rate: u32) -> Result<String, String> {
    let cable = render_endpoints().map_err(|e| e.to_string())?.into_iter().find(|e| e.name.starts_with(CABLE)).ok_or("VB-Cable not found")?;
    let format = |bits: u16, sub: GUID| WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: 0xFFFE,
            nChannels: 2,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * 2 * (bits as u32 / 8),
            nBlockAlign: 2 * bits / 8,
            wBitsPerSample: bits,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: bits },
        dwChannelMask: 0x3,
        SubFormat: sub,
    };
    let (device, mix) = (format(24, KSDATAFORMAT_SUBTYPE_PCM), format(32, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT));
    let id = HSTRING::from(cable.id.as_str());
    // SAFETY: both formats outlive the call, which copies them.
    unsafe {
        let policy: IPolicyConfig = CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
        set_device_format(&policy, PCWSTR(id.as_ptr()), addr_of!(device).cast(), addr_of!(mix).cast())
            .ok()
            .map_err(|e| format!("could not set the format: {e}"))?;
    }
    let now = render_endpoints().map_err(|e| e.to_string())?.into_iter().find(|e| e.id == cable.id).map(|e| e.sample_rate);
    match now {
        Some(r) if r == rate => Ok(format!("Cable set to 24-bit / {} kHz.", rate as f64 / 1000.0)),
        other => Err(format!("format set, but the cable reports {other:?} Hz")),
    }
}

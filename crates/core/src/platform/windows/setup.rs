//! One-click fixes: installing VB-Cable, its sample rate, routing the app to it.

use std::io::Cursor;
use std::path::Path;
use std::ptr::addr_of;

use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::Media::Audio::{EDataFlow, ERole, WAVEFORMATEX, eConsole, eMultimedia, eRender};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, PCWSTR, w};
use windows_core::interface;

use super::devices::{com_init, render_endpoints, wave_format};
use crate::devices::Endpoint;
use crate::profile::Profile;
use crate::sessions;
use crate::setup::{Check, State, check, spotify_checks};

const CABLE: &str = "CABLE Input";
const CABLE_ZIP: &str = "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip";

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
            check("cable_rate", "Cable sample rate", State::Ok, format!("{khz} kHz – {}'s rate, no resampling", profile.name), None)
        } else {
            check("cable_rate", "Cable sample rate", State::Bad, format!("{} Hz – {} ({khz} kHz) would be resampled", cable.sample_rate, profile.name), Some("cable_rate"))
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
                format!("{} plays on \"{name}\" (effects and resampling alter it). Route it to the cable – or by hand: {}", profile.name, profile.output_hint),
                cable.map(|_| "route_cable"),
            ),
            None => check("route", "App output", State::Check, format!("{} has no audio stream yet – play something once", profile.name), None),
        });
    } else {
        out.push(check("route", "App output", State::Check, format!("{} is not running", profile.name), None));
    }

    if profile.id == "spotify" {
        out.extend(spotify_checks());
    }
    if !sessions::listen_volumes().is_empty() {
        out.push(check("listen", "\"Listen to this device\"", State::Ok, "Active – Mitschnitt mutes it while listening and monitors itself", None));
    }
    out
}

/// Returns a message for the user.
pub fn fix(action: &str, profile: &Profile) -> Result<String, String> {
    com_init();
    match action {
        "install_cable" => install_cable(profile.rate),
        "cable_rate" => set_cable_rate(profile.rate),
        "route_cable" => route_to_cable(profile),
        other => Err(format!("unknown fix \"{other}\"")),
    }
}

/// Like Windows' "App volume and device preferences" page: a per-app default device that Windows remembers.
/// Only applies when the app plays on its "default" output.
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

/// Undocumented WinRT factory behind Windows' per-app device page (Windows 10 21H2+ layout). Only the vtable
/// order matters; the unused methods are placeholders.
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

/// Downloads the official package and runs VB-Audio's installer elevated; nothing of VB-Audio is shipped with
/// Mitschnitt.
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
        Err(format!("the installer finished (code {code}) but no cable is visible yet – a restart of Windows may be needed"))
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

/// Undocumented but long-stable interface of the Sound control panel; only the vtable order matters.
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

/// 2 ch / 24-bit / `rate`, engine mix 32-bit float.
fn set_cable_rate(rate: u32) -> Result<String, String> {
    let cable = render_endpoints().map_err(|e| e.to_string())?.into_iter().find(|e| e.name.starts_with(CABLE)).ok_or("VB-Cable not found")?;
    let (device, mix) = (wave_format(rate, 2, 24, KSDATAFORMAT_SUBTYPE_PCM), wave_format(rate, 2, 32, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT));
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

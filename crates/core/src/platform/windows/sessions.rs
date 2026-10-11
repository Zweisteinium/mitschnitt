//! Processes and audio sessions: find an app's process tree and the device it plays on, and read or set
//! its volume-mixer value (plus the "Listen to this device" stream of a virtual cable).

use std::collections::HashSet;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2, IMMDevice,
    ISimpleAudioVolume, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoTaskMemFree};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::core::Interface;

use super::devices::{describe, enumerator};
use crate::devices::Endpoint;
use crate::sessions::{Process, find_app, tree};

pub fn processes() -> Vec<Process> {
    let mut out = Vec::new();
    // SAFETY: toolhelp snapshot owned and closed here; the entry struct is properly sized.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snapshot, &mut entry).is_ok();
        while ok {
            let len = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
            out.push(Process {
                pid: entry.th32ProcessID,
                parent: entry.th32ParentProcessID,
                name: String::from_utf16_lossy(&entry.szExeFile[..len]),
            });
            ok = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    out
}

fn for_each_session(mut f: impl FnMut(&IMMDevice, &IAudioSessionControl2)) -> windows::core::Result<()> {
    // SAFETY: COM calls on interfaces we own.
    unsafe {
        let devices = enumerator()?.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for i in 0..devices.GetCount()? {
            let device = devices.Item(i)?;
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
            let sessions = manager.GetSessionEnumerator()?;
            for j in 0..sessions.GetCount()? {
                if let Ok(control) = sessions.GetSession(j)?.cast::<IAudioSessionControl2>() {
                    f(&device, &control);
                }
            }
        }
    }
    Ok(())
}

/// Volume-mixer controls of the process tree's sessions (all devices).
pub fn app_volumes(tree: &HashSet<u32>) -> Vec<ISimpleAudioVolume> {
    let mut out = Vec::new();
    let _ = for_each_session(|_, control| {
        // SAFETY: querying our own session control.
        if unsafe { control.GetProcessId() }.is_ok_and(|pid| tree.contains(&pid)) {
            if let Ok(volume) = control.cast::<ISimpleAudioVolume>() {
                out.push(volume);
            }
        }
    });
    out
}

/// "Listen to this device" streams of the VB-Cable output (what you hear while the app plays into the cable).
pub fn listen_volumes() -> Vec<ISimpleAudioVolume> {
    let mut out = Vec::new();
    let _ = for_each_session(|_, control| {
        // SAFETY: the display name string is allocated by COM and freed here.
        let name = unsafe {
            control.GetDisplayName().map(|p| {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as _));
                s
            })
        };
        if name.is_ok_and(|n| n.contains("CABLE Output")) {
            if let Ok(volume) = control.cast::<ISimpleAudioVolume>() {
                out.push(volume);
            }
        }
    });
    out
}

/// The output device the process tree plays on (an active session wins over an idle one).
pub fn app_endpoint(tree: &HashSet<u32>) -> Option<Endpoint> {
    let (mut active, mut idle) = (None, None);
    let _ = for_each_session(|device, control| {
        // SAFETY: querying our own session control.
        let (pid, state) = unsafe { (control.GetProcessId(), control.GetState()) };
        if pid.is_ok_and(|pid| tree.contains(&pid)) {
            let slot = if state.is_ok_and(|s| s == AudioSessionStateActive) { &mut active } else { &mut idle };
            if slot.is_none() {
                *slot = Some(device.clone());
            }
        }
    });
    active.or(idle).and_then(|d| describe(&d).ok())
}

/// The volume-mixer controls of an app, looked up afresh on every call - a restarted app gets new sessions,
/// and controls obtained earlier would silently point at the dead ones.
pub fn app_volumes_by_name(app: &str) -> Vec<ISimpleAudioVolume> {
    let procs = processes();
    find_app(app, &procs).map(|root| app_volumes(&tree(root.pid, &procs))).unwrap_or_default()
}

pub fn get(controls: &[ISimpleAudioVolume]) -> Vec<f32> {
    // SAFETY: reading our own controls.
    controls.iter().filter_map(|c| unsafe { c.GetMasterVolume() }.ok()).collect()
}

pub fn set(controls: &[ISimpleAudioVolume], value: f32) {
    for control in controls {
        // SAFETY: setting our own control.
        let _ = unsafe { control.SetMasterVolume(value, std::ptr::null()) };
    }
}

pub fn muted(controls: &[ISimpleAudioVolume]) -> Vec<bool> {
    // SAFETY: reading our own controls.
    controls.iter().filter_map(|c| unsafe { c.GetMute() }.ok().map(|m| m.as_bool())).collect()
}

pub fn set_mute(controls: &[ISimpleAudioVolume], mute: bool) {
    for control in controls {
        // SAFETY: setting our own control.
        let _ = unsafe { control.SetMute(mute, std::ptr::null()) };
    }
}

pub fn is_running(pid: u32) -> bool {
    processes().iter().any(|p| p.pid == pid)
}

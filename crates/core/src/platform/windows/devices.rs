//! Windows render endpoints: name, id and the audio engine's mix format.

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, STGM_READ,
};

use crate::devices::Endpoint;

/// Initialise COM (multithreaded) on the calling thread; harmless if it already is.
pub fn com_init() {
    // SAFETY: plain COM initialisation; a failure means it was already initialised.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

pub(crate) fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
    // SAFETY: creating the system device enumerator.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

/// All active output devices with their engine (mix) format.
pub fn render_endpoints() -> windows::core::Result<Vec<Endpoint>> {
    com_init();
    // SAFETY: COM calls on interfaces we own.
    unsafe {
        let collection = enumerator()?.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        (0..collection.GetCount()?).map(|i| describe(&collection.Item(i)?)).collect()
    }
}

/// The default output device.
pub fn default_endpoint() -> windows::core::Result<Endpoint> {
    com_init();
    // SAFETY: COM calls on interfaces we own.
    unsafe { describe(&enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole)?) }
}

pub(crate) fn describe(device: &IMMDevice) -> windows::core::Result<Endpoint> {
    // SAFETY: COM calls on interfaces we own; strings and formats allocated by COM are freed here.
    unsafe {
        let id = device.GetId()?;
        let id_string = id.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id.0 as _));
        let name = device.OpenPropertyStore(STGM_READ)?.GetValue(&PKEY_Device_FriendlyName)?.to_string();
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = client.GetMixFormat()?;
        let (sample_rate, channels) = ((*format).nSamplesPerSec, (*format).nChannels);
        CoTaskMemFree(Some(format as _));
        Ok(Endpoint { id: id_string, name, sample_rate, channels })
    }
}

/// The cable runs at the app's rate already (Setup tab): nothing to hold.
pub fn hold_rate(_rate: u32) -> Result<Option<u32>, String> {
    Ok(None)
}

pub fn release_rate(_previous: u32) {}

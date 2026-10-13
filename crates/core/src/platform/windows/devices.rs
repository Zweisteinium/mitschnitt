use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, STGM_READ,
};
use windows::core::GUID;

use crate::devices::Endpoint;

pub fn com_init() {
    // SAFETY: plain COM initialisation; a failure means it was already initialised.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

pub(crate) fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
    // SAFETY: creating the system device enumerator.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

pub fn render_endpoints() -> windows::core::Result<Vec<Endpoint>> {
    com_init();
    // SAFETY: COM calls on interfaces we own.
    unsafe {
        let collection = enumerator()?.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        (0..collection.GetCount()?).map(|i| describe(&collection.Item(i)?)).collect()
    }
}

pub fn default_endpoint() -> windows::core::Result<Endpoint> {
    com_init();
    // SAFETY: COM calls on interfaces we own.
    unsafe { describe(&enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole)?) }
}

pub(crate) fn describe(device: &IMMDevice) -> windows::core::Result<Endpoint> {
    // SAFETY: COM calls on interfaces we own; the format allocated by COM is freed here.
    unsafe {
        let name = device.OpenPropertyStore(STGM_READ)?.GetValue(&PKEY_Device_FriendlyName)?.to_string();
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = client.GetMixFormat()?;
        let (sample_rate, channels) = ((*format).nSamplesPerSec, (*format).nChannels);
        CoTaskMemFree(Some(format as _));
        Ok(Endpoint { id: device_id(device)?, name, sample_rate, channels })
    }
}

pub(crate) fn device_id(device: &IMMDevice) -> windows::core::Result<String> {
    // SAFETY: the id string is allocated by COM and freed here.
    unsafe {
        let id = device.GetId()?;
        let s = id.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id.0 as _));
        Ok(s)
    }
}

pub(crate) fn wave_format(rate: u32, channels: u16, bits: u16, sub: GUID) -> WAVEFORMATEXTENSIBLE {
    let block = channels * bits / 8;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: 0xFFFE, // WAVE_FORMAT_EXTENSIBLE
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * block as u32,
            nBlockAlign: block,
            wBitsPerSample: bits,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: bits },
        dwChannelMask: if channels == 1 { 0x4 } else { 0x3 },
        SubFormat: sub,
    }
}

/// The cable runs at the app's rate already (Setup tab): nothing to hold.
pub fn hold_rate(_rate: u32) -> Result<Option<u32>, String> {
    Ok(None)
}

pub fn release_rate(_previous: u32) {}

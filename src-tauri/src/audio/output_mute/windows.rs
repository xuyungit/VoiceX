//! Windows Core Audio: the mute switch of the default render endpoint's
//! `IAudioEndpointVolume`, the one the taskbar volume flyout toggles.
//!
//! Windows mutes in its audio engine when the hardware cannot, so every
//! active render endpoint reads as settable. Endpoints are addressed by their
//! endpoint id, which survives a Bluetooth device reconnecting; a known
//! endpoint that is not active (unplugged, disabled) reads as absent.
//!
//! Runs on the output mute worker thread, which joins the multithreaded COM
//! apartment once and keeps its device enumerator for the life of the process.

use windows::core::{HSTRING, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};

use super::{Backend, DeviceMute, OutputDevice};

pub struct PlatformBackend {
    enumerator: Option<IMMDeviceEnumerator>,
}

impl PlatformBackend {
    pub fn new() -> Self {
        Self { enumerator: None }
    }

    fn enumerator(&mut self) -> Result<IMMDeviceEnumerator, String> {
        if let Some(enumerator) = &self.enumerator {
            return Ok(enumerator.clone());
        }
        // S_FALSE ("already initialized") is success.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|err| format!("joining the COM apartment failed: {err}"))?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(|err| format!("creating the device enumerator failed: {err}"))?;
        self.enumerator = Some(enumerator.clone());
        Ok(enumerator)
    }

    /// `None` when Windows does not know the endpoint at all.
    fn device(&mut self, id: &str) -> Result<Option<IMMDevice>, String> {
        match unsafe { self.enumerator()?.GetDevice(&HSTRING::from(id)) } {
            Ok(device) => Ok(Some(device)),
            Err(err) if err.code() == ERROR_NOT_FOUND.to_hresult() => Ok(None),
            Err(err) => Err(format!("opening the endpoint failed: {err}")),
        }
    }

    /// The endpoint's volume control, or `None` when it is not active.
    fn volume(&mut self, id: &str) -> Result<Option<IAudioEndpointVolume>, String> {
        let Some(device) = self.device(id)? else {
            return Ok(None);
        };
        let state = unsafe { device.GetState() }
            .map_err(|err| format!("reading the endpoint state failed: {err}"))?;
        if state != DEVICE_STATE_ACTIVE {
            return Ok(None);
        }
        unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) }
            .map(Some)
            .map_err(|err| format!("opening the endpoint volume failed: {err}"))
    }
}

fn endpoint_id(device: &IMMDevice) -> Result<String, String> {
    let raw: PWSTR = unsafe { device.GetId() }
        .map_err(|err| format!("reading the endpoint id failed: {err}"))?;
    let id = unsafe { raw.to_string() };
    unsafe { CoTaskMemFree(Some(raw.0 as *const _)) };
    id.map_err(|err| format!("endpoint id is not valid UTF-16: {err}"))
}

fn friendly_name(device: &IMMDevice) -> Result<String, String> {
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.map_err(|err| err.to_string())?;
    let value =
        unsafe { store.GetValue(&PKEY_Device_FriendlyName) }.map_err(|err| err.to_string())?;
    Ok(value.to_string())
}

impl Backend for PlatformBackend {
    fn default_output(&mut self) -> Result<Option<OutputDevice>, String> {
        let device = match unsafe {
            self.enumerator()?
                .GetDefaultAudioEndpoint(eRender, eConsole)
        } {
            Ok(device) => device,
            Err(err) if err.code() == ERROR_NOT_FOUND.to_hresult() => return Ok(None),
            Err(err) => return Err(format!("reading the default output failed: {err}")),
        };
        let id = endpoint_id(&device)?;
        let name = friendly_name(&device)
            .ok()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| id.clone());
        Ok(Some(OutputDevice { id, name }))
    }

    fn read_mute(&mut self, id: &str) -> Result<DeviceMute, String> {
        let Some(volume) = self.volume(id)? else {
            return Ok(DeviceMute::Absent);
        };
        let muted = unsafe { volume.GetMute() }
            .map_err(|err| format!("reading the mute switch failed: {err}"))?;
        Ok(DeviceMute::Settable {
            muted: muted.as_bool(),
        })
    }

    fn set_mute(&mut self, id: &str, muted: bool) -> Result<(), String> {
        let Some(volume) = self.volume(id)? else {
            return Err("the endpoint is not active".to_string());
        };
        unsafe { volume.SetMute(muted, std::ptr::null()) }
            .map_err(|err| format!("setting the mute switch failed: {err}"))
    }
}

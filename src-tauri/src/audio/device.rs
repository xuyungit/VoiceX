//! Audio input device manager
//!
//! Enumerates and manages audio input devices.

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::Device;
use serde::{Deserialize, Serialize};

use super::capture::AudioCaptureError;

/// Represents an audio input device
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDevice {
    pub uid: String,
    pub name: String,
    pub is_default: bool,
}

/// Special UID representing "follow system default"
pub const SYSTEM_DEFAULT_UID: &str = "__system_default__";

/// Manages audio input devices
pub struct AudioInputDeviceManager;

impl AudioInputDeviceManager {
    /// Get list of available input devices, with "System Default" as first option
    pub fn get_devices() -> Vec<AudioDevice> {
        let host = cpal::default_host();
        let default_name = host.default_input_device().and_then(|d| d.name().ok());

        let mut result = vec![AudioDevice {
            uid: SYSTEM_DEFAULT_UID.to_string(),
            name: "System Default".to_string(),
            is_default: true, // System Default is always marked as "default" option
        }];

        match host.input_devices() {
            Ok(devices) => {
                result.extend(devices.filter_map(|device| {
                    let name = device.name().ok()?;
                    let is_current_default =
                        default_name.as_ref().map(|n| n == &name).unwrap_or(false);

                    Some(AudioDevice {
                        uid: name.clone(),
                        name: if is_current_default {
                            format!("{} (Current Default)", name)
                        } else {
                            name
                        },
                        is_default: false, // Only the virtual "System Default" is marked as default
                    })
                }));
            }
            Err(err) => {
                log::warn!("Failed to enumerate input devices: {}", err);
            }
        }

        result
    }

    /// Get the default input device
    pub fn get_default_device() -> Option<AudioDevice> {
        Self::get_devices().into_iter().find(|d| d.is_default)
    }

    /// Resolve an input device by UID (or default if missing/system default selected)
    pub fn resolve_device(preferred_uid: Option<&str>) -> Result<Device, AudioCaptureError> {
        let host = cpal::default_host();

        // If no preference or explicitly "System Default", use the system default device
        if preferred_uid.is_none() || preferred_uid == Some(SYSTEM_DEFAULT_UID) {
            if let Some(default) = host.default_input_device() {
                return Ok(default);
            }
            // Fall through to try any available device
        }

        let mut devices = host
            .input_devices()
            .map_err(|e| AudioCaptureError::StartFailed(format!("Failed to list devices: {}", e)))?
            .collect::<Vec<_>>();

        if let Some(uid) = preferred_uid {
            if uid != SYSTEM_DEFAULT_UID {
                if let Some(device) = devices.iter().find_map(|device| {
                    let name = device.name().ok()?;
                    if name == uid {
                        Some(device.clone())
                    } else {
                        None
                    }
                }) {
                    return Ok(device);
                }
                log::warn!(
                    "Preferred device UID {} not found, falling back to default",
                    uid
                );
            }
        }

        // Final fallback: system default or any available device
        if let Some(default) = host.default_input_device() {
            return Ok(default);
        }

        devices.pop().ok_or(AudioCaptureError::NoDevice)
    }
}

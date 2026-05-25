//! Local audio output device discovery for the "media output selection"
//! feature. Names returned here match what the librespot rodio backend expects
//! as its device string, so selecting one and passing it through works directly.

use cpal::traits::{DeviceTrait, HostTrait};

use crate::model::OutputDevice;

/// Enumerate output devices on the default host. The system default is flagged
/// and always placed first. Errors are swallowed into an empty list because the
/// UI treats "no devices" as "use system default".
pub fn output_devices() -> Vec<OutputDevice> {
    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .and_then(|d| d.name().ok());

    let mut devices = Vec::new();
    if let Some(name) = &default_name {
        devices.push(OutputDevice {
            name: name.clone(),
            is_default: true,
        });
    }

    if let Ok(iter) = host.output_devices() {
        for dev in iter {
            if let Ok(name) = dev.name() {
                if Some(&name) == default_name.as_ref() {
                    continue; // already added as the default entry
                }
                devices.push(OutputDevice {
                    name,
                    is_default: false,
                });
            }
        }
    }
    devices
}

use cpal::traits::{DeviceTrait, HostTrait};

use crate::device::descriptor::DeviceDescriptor;

pub fn enumerate_all() -> Vec<DeviceDescriptor> {
    let host = cpal::default_host();
    let mut descriptors = Vec::new();

    match host.input_devices() {
        Ok(devices) => {
            for device in devices {
                if let Some(desc) = describe_device(&device, true, false) {
                    descriptors.push(desc);
                }
            }
        }
        Err(e) => log::warn!("failed to enumerate input devices: {e}"),
    }

    match host.output_devices() {
        Ok(devices) => {
            for device in devices {
                if let Some(desc) = describe_device(&device, false, true) {
                    // Avoid duplicating duplex devices already added as inputs
                    if !descriptors.iter().any(|d: &DeviceDescriptor| d.name == desc.name) {
                        descriptors.push(desc);
                    } else if let Some(existing) = descriptors.iter_mut().find(|d| d.name == desc.name) {
                        existing.is_output = true;
                    }
                }
            }
        }
        Err(e) => log::warn!("failed to enumerate output devices: {e}"),
    }

    descriptors
}

fn describe_device(device: &cpal::Device, is_input: bool, is_output: bool) -> Option<DeviceDescriptor> {
    let name = device.name().ok()?;

    let (sample_rate, channel_count) = if is_input {
        let cfg = device.default_input_config().ok()?;
        (cfg.sample_rate().0, cfg.channels())
    } else {
        let cfg = device.default_output_config().ok()?;
        (cfg.sample_rate().0, cfg.channels())
    };

    Some(DeviceDescriptor {
        name,
        is_input,
        is_output,
        default_sample_rate: sample_rate,
        channel_count,
    })
}

//! The default sink's monitor, which PipeWire and PulseAudio both expose as a capture device
//! named like `*.monitor`. A window has no separate node here.
//! shortcut: default-sink monitor, not one window's PipeWire node, upgrade when a share's node id is known.

use super::{cpal_input, Pull};
use cpal::traits::{DeviceTrait, HostTrait};

pub(super) fn open(screen: Option<&str>) -> Result<Box<dyn Pull>, String> {
    let host = cpal::default_host();
    let devices = host.input_devices().map_err(|err| err.to_string())?;
    let mut monitors = Vec::new();
    for device in devices {
        let name = device.name().unwrap_or_default();
        if name.to_ascii_lowercase().contains("monitor") {
            monitors.push((name, device));
        }
    }
    if monitors.is_empty() {
        return Err("No PipeWire or PulseAudio monitor is available to share sound.".to_owned());
    }
    let hint = screen.unwrap_or("");
    let at = monitors
        .iter()
        .position(|(name, _)| !hint.is_empty() && name.contains(hint))
        .or_else(|| monitors.iter().position(|(name, _)| name.contains(".monitor")))
        .unwrap_or(0);
    let (name, device) = monitors.swap_remove(at);
    tracing::debug!("mediad: share audio from {name}");
    let supported = device.default_input_config().map_err(|err| err.to_string())?;
    cpal_input(&device, supported).map(|device| Box::new(device) as Box<dyn Pull>)
}

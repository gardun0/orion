use serde::{Deserialize, Serialize};

/// What the active audio backend can actually do on this platform, reported
/// at runtime when the backend starts. The UI gates affordances on these
/// flags instead of assuming every platform supports every feature: virtual
/// devices are native on PipeWire, need an Audio Server Plugin on macOS, and
/// need a signed audio driver on Windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BackendCapabilities {
    /// The backend can create and manage virtual input/output devices.
    pub virtual_devices: bool,
    /// The backend can expose individual applications as capturable source
    /// endpoints (PipeWire stream nodes, WASAPI process loopback sessions,
    /// Core Audio process taps).
    pub application_sources: bool,
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod cpal_backend;
mod fake;
#[cfg(target_os = "macos")]
mod macos_tap;
#[cfg(target_os = "linux")]
mod pipewire;
#[cfg(target_os = "linux")]
mod streams;
#[cfg(target_os = "windows")]
mod windows_loopback;

use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

use crate::domain::{
    AudioEndpoint, AudioError, AudioRoute, BackendCapabilities, CommandId, EndpointId, GainDb,
    MeterFrame, NormalizedBalance, RouteId, VirtualDeviceId,
};

pub use fake::FakeBackend;
#[cfg(target_os = "linux")]
pub use pipewire::PipeWireBackend;

/// The platform's backend: PipeWire on Linux (strictly richer: virtual
/// devices, app streams, graph routing), cpal on Windows/macOS.
#[cfg(target_os = "linux")]
pub fn default_backend() -> PipeWireBackend {
    PipeWireBackend
}

/// The platform's backend: PipeWire on Linux (strictly richer: virtual
/// devices, app streams, graph routing), cpal on Windows/macOS.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn default_backend() -> cpal_backend::CpalBackend {
    cpal_backend::CpalBackend
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendCommand {
    SetVolume {
        command_id: CommandId,
        endpoint_id: EndpointId,
        gain: GainDb,
    },
    SetMute {
        command_id: CommandId,
        endpoint_id: EndpointId,
        muted: bool,
    },
    SetBalance {
        command_id: CommandId,
        endpoint_id: EndpointId,
        balance: NormalizedBalance,
    },
    Connect {
        command_id: CommandId,
        route: AudioRoute,
    },
    Disconnect {
        command_id: CommandId,
        route_id: RouteId,
    },
    CreateVirtual {
        command_id: CommandId,
        virtual_device_id: VirtualDeviceId,
        endpoint: Box<AudioEndpoint>,
    },
    DeleteVirtual {
        command_id: CommandId,
        virtual_device_id: VirtualDeviceId,
    },
    SetStreamTuning {
        command_id: CommandId,
        stream_rate: u32,
        buffer_frames: u32,
    },
    SetDelay {
        command_id: CommandId,
        endpoint_id: EndpointId,
        delay_ms: f32,
    },
    SetEq {
        command_id: CommandId,
        endpoint_id: EndpointId,
        low_db: f32,
        mid_db: f32,
        high_db: f32,
    },
    SetChannelMode {
        command_id: CommandId,
        endpoint_id: EndpointId,
        mode: crate::domain::ChannelMode,
    },
    Shutdown {
        command_id: CommandId,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendEvent {
    Status {
        status: BackendStatus,
    },
    /// What this backend can do on the running platform; reported once at
    /// startup so the UI can gate affordances (e.g. virtual devices).
    Capabilities {
        capabilities: BackendCapabilities,
    },
    EndpointAdded {
        endpoint: AudioEndpoint,
    },
    EndpointUpdated {
        endpoint: AudioEndpoint,
    },
    EndpointRemoved {
        endpoint_id: EndpointId,
    },
    RouteAdded {
        route: AudioRoute,
    },
    RouteUpdated {
        route: AudioRoute,
    },
    RouteRemoved {
        route_id: RouteId,
    },
    Meter {
        frame: MeterFrame,
    },
    /// PipeWire graph clock detected from the `settings` metadata.
    AudioSettings {
        rate: u32,
        quantum: u32,
        min_quantum: u32,
        max_quantum: u32,
    },
    CommandCompleted {
        command_id: CommandId,
    },
    CommandFailed {
        command_id: CommandId,
        error: AudioError,
    },
    Error {
        error: AudioError,
    },
    ShutdownComplete {
        command_id: CommandId,
    },
}

pub trait AudioBackend: Send + 'static {
    fn run(
        self: Box<Self>,
        commands: Receiver<BackendCommand>,
        events: Sender<BackendEvent>,
    ) -> Result<(), AudioError>;
}

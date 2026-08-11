use serde::{Deserialize, Serialize};

use super::{
    AudioEndpoint, CommandId, EndpointId, GainDb, NormalizedBalance, RouteId, VirtualDeviceId,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineCommand {
    pub id: CommandId,
    pub kind: EngineCommandKind,
}

impl EngineCommand {
    pub fn new(kind: EngineCommandKind) -> Self {
        Self {
            id: CommandId::new(),
            kind,
        }
    }

    pub fn shutdown() -> Self {
        Self::new(EngineCommandKind::Shutdown)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineCommandKind {
    SetVolume {
        endpoint_id: EndpointId,
        gain: GainDb,
    },
    SetMute {
        endpoint_id: EndpointId,
        muted: bool,
    },
    SetBalance {
        endpoint_id: EndpointId,
        balance: NormalizedBalance,
    },
    Connect {
        route_id: RouteId,
        source: EndpointId,
        destination: EndpointId,
    },
    Disconnect {
        route_id: RouteId,
    },
    CreateVirtual {
        virtual_device_id: VirtualDeviceId,
        endpoint: Box<AudioEndpoint>,
    },
    DeleteVirtual {
        virtual_device_id: VirtualDeviceId,
    },
    /// Live stream tuning from Settings: sample rate and latency hint for
    /// route streams (Orion-local). Applied immediately: the backend rebuilds
    /// active route streams so the change takes effect without re-patching.
    SetStreamTuning {
        stream_rate: u32,
        buffer_frames: u32,
    },
    /// Per-endpoint sync delay in milliseconds: on a source it delays the
    /// captured audio into every route (e.g. aligning a fast mic to a delayed
    /// video capture); on an output it delays every route playing into it.
    SetDelay {
        endpoint_id: EndpointId,
        delay_ms: f32,
    },
    /// Per-endpoint 3-band EQ gains in dB (low shelf / mid bell / high
    /// shelf), applied to every route touching that endpoint.
    SetEq {
        endpoint_id: EndpointId,
        low_db: f32,
        mid_db: f32,
        high_db: f32,
    },
    /// How the endpoint's channels map into the mix (mono downmix, single
    /// channel, swap, ...). Applies live to every route touching it.
    SetChannelMode {
        endpoint_id: EndpointId,
        mode: super::ChannelMode,
    },
    Shutdown,
}

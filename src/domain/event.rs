use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{
    AudioEndpoint, AudioError, AudioGraph, AudioRoute, BackendCapabilities, ChannelId, CommandId,
    EndpointId, MeterLevel, RouteId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GraphDelta {
    EndpointAdded { endpoint: AudioEndpoint },
    EndpointUpdated { endpoint: AudioEndpoint },
    EndpointRemoved { endpoint_id: EndpointId },
    RouteAdded { route: AudioRoute },
    RouteUpdated { route: AudioRoute },
    RouteRemoved { route_id: RouteId },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeterFrame {
    pub endpoint_id: EndpointId,
    pub sequence: u64,
    pub levels: HashMap<ChannelId, MeterLevel>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    Snapshot {
        graph: AudioGraph,
    },
    Delta {
        delta: Box<GraphDelta>,
    },
    Status {
        status: EngineStatus,
    },
    /// Runtime-reported feature set of the active backend; the UI gates
    /// affordances (virtual devices) on it.
    Capabilities {
        capabilities: BackendCapabilities,
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
    Error {
        command_id: Option<CommandId>,
        error: AudioError,
    },
}

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{ChannelId, DeviceId, EndpointId, GainDb, NormalizedBalance, VirtualDeviceId};

/// Namespace for all derived stable IDs. Both platform backends derive
/// endpoint identities with these helpers so a persisted route survives
/// restarts and hot-plug regardless of the OS audio stack.
const ORION_ID_NAMESPACE: Uuid = Uuid::from_u128(0x56ef_da46_0a0a_4f64_a31e_3f76_74fc_b476);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointIdentity {
    pub backend: String,
    pub serial: Option<String>,
    pub bus: Option<String>,
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub device_name: Option<String>,
    pub node_name: Option<String>,
    pub profile: Option<String>,
    pub media_class: Option<String>,
    pub alsa_path: Option<String>,
}

impl EndpointIdentity {
    pub fn new(backend: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            serial: None,
            bus: None,
            vendor: None,
            product: None,
            device_name: None,
            node_name: None,
            profile: None,
            media_class: None,
            alsa_path: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EndpointType {
    PhysicalInput,
    PhysicalOutput,
    ApplicationInput,
    ApplicationOutput,
    VirtualInput,
    VirtualOutput,
    InternalBus,
}

impl EndpointType {
    pub const fn can_source(self) -> bool {
        matches!(
            self,
            Self::PhysicalInput | Self::ApplicationOutput | Self::VirtualInput | Self::InternalBus
        )
    }

    pub const fn can_receive(self) -> bool {
        matches!(
            self,
            Self::PhysicalOutput | Self::ApplicationInput | Self::VirtualOutput | Self::InternalBus
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointState {
    Available,
    Suspended,
    Idle,
    Running,
    Disconnected,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioEndpoint {
    pub id: EndpointId,
    #[serde(skip, default)]
    pub runtime_id: Option<u32>,
    pub device_id: Option<DeviceId>,
    pub virtual_device_id: Option<VirtualDeviceId>,
    pub identity: EndpointIdentity,
    pub name: String,
    pub description: String,
    pub endpoint_type: EndpointType,
    pub state: EndpointState,
    pub channel_count: u32,
    pub sample_rate: Option<u32>,
    pub is_default: bool,
    pub channels: Vec<ChannelId>,
    pub gain: GainDb,
    pub muted: bool,
    pub balance: NormalizedBalance,
}

/// Stable endpoint identity derived from its identity fingerprint and
/// direction. The key format is frozen: persisted sessions and channel IDs
/// depend on it.
pub fn stable_endpoint_id(identity: &EndpointIdentity, endpoint_type: EndpointType) -> EndpointId {
    let key = format!(
        "{:?}|{}|{}|{}|{}|{}|{}|{}",
        endpoint_type,
        identity.serial.as_deref().unwrap_or(""),
        identity.bus.as_deref().unwrap_or(""),
        identity.vendor.as_deref().unwrap_or(""),
        identity.product.as_deref().unwrap_or(""),
        identity.device_name.as_deref().unwrap_or(""),
        identity.node_name.as_deref().unwrap_or(""),
        identity.profile.as_deref().unwrap_or("")
    );
    EndpointId::from_uuid(Uuid::new_v5(&ORION_ID_NAMESPACE, key.as_bytes()))
}

pub fn stable_device_id(identity: &EndpointIdentity) -> Option<DeviceId> {
    let key = identity
        .serial
        .as_deref()
        .or(identity.device_name.as_deref())?;
    Some(DeviceId::from_uuid(Uuid::new_v5(
        &ORION_ID_NAMESPACE,
        format!("device|{key}").as_bytes(),
    )))
}

pub fn stable_channel_id(endpoint_id: EndpointId, index: u32) -> ChannelId {
    ChannelId::from_uuid(Uuid::new_v5(
        &ORION_ID_NAMESPACE,
        format!("channel|{endpoint_id}|{index}").as_bytes(),
    ))
}

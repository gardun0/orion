use serde::{Deserialize, Serialize};

use super::{EndpointId, RouteId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteState {
    Connecting,
    Active,
    Disconnecting,
    Disconnected,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioRoute {
    pub id: RouteId,
    pub source: EndpointId,
    pub destination: EndpointId,
    pub state: RouteState,
}

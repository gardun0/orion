use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        #[schemars(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

define_id!(DeviceId);
define_id!(EndpointId);
define_id!(RouteId);
define_id!(ChannelId);
define_id!(VirtualDeviceId);
define_id!(CommandId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_distinct_and_display_as_uuids() {
        let device = DeviceId::new();
        let endpoint = EndpointId::new();

        assert_ne!(device.to_string(), endpoint.to_string());
        assert!(Uuid::parse_str(&device.to_string()).is_ok());
    }

    #[test]
    fn ids_round_trip_through_serde() {
        let id = RouteId::new();
        let encoded = match serde_json::to_string(&id) {
            Ok(encoded) => encoded,
            Err(error) => panic!("failed to serialize route id: {error}"),
        };
        let decoded: RouteId = match serde_json::from_str(&encoded) {
            Ok(decoded) => decoded,
            Err(error) => panic!("failed to deserialize route id: {error}"),
        };

        assert_eq!(decoded, id);
    }
}

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{
    AudioEndpoint, AudioError, AudioRoute, EndpointId, EndpointState, ErrorCode, ErrorSeverity,
    RouteId, RouteState,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioGraph {
    pub endpoints: HashMap<EndpointId, AudioEndpoint>,
    pub routes: HashMap<RouteId, AudioRoute>,
}

impl AudioGraph {
    pub fn add_endpoint(&mut self, endpoint: AudioEndpoint) -> Result<(), AudioError> {
        if self.endpoints.contains_key(&endpoint.id) {
            return Err(graph_error(
                ErrorCode::DuplicateEndpoint,
                "That device is already present.",
                format!("duplicate endpoint {}", endpoint.id),
            ));
        }
        self.endpoints.insert(endpoint.id, endpoint);
        Ok(())
    }

    pub fn upsert_endpoint(&mut self, endpoint: AudioEndpoint) {
        self.endpoints.insert(endpoint.id, endpoint);
    }

    pub fn add_route(&mut self, route: AudioRoute) -> Result<(), AudioError> {
        if self.routes.contains_key(&route.id) {
            return Err(graph_error(
                ErrorCode::DuplicateRoute,
                "That connection already exists.",
                format!("duplicate route {}", route.id),
            ));
        }
        self.validate_route(&route)?;
        if self.routes.values().any(|existing| {
            existing.source == route.source && existing.destination == route.destination
        }) {
            return Err(graph_error(
                ErrorCode::DuplicateRoute,
                "Those devices are already connected.",
                format!(
                    "duplicate route from {} to {}",
                    route.source, route.destination
                ),
            ));
        }
        self.routes.insert(route.id, route);
        Ok(())
    }

    pub fn upsert_route(&mut self, route: AudioRoute) -> Result<(), AudioError> {
        self.validate_route(&route)?;
        let duplicate = self.routes.values().any(|existing| {
            existing.id != route.id
                && existing.source == route.source
                && existing.destination == route.destination
        });
        if duplicate {
            return Err(graph_error(
                ErrorCode::DuplicateRoute,
                "Those devices are already connected.",
                format!(
                    "duplicate route from {} to {}",
                    route.source, route.destination
                ),
            ));
        }
        self.routes.insert(route.id, route);
        Ok(())
    }

    pub fn disconnect_endpoint(&mut self, id: EndpointId) -> Result<(), AudioError> {
        let endpoint = self.endpoints.get_mut(&id).ok_or_else(|| {
            graph_error(
                ErrorCode::EndpointNotFound,
                "That device is no longer available.",
                format!("endpoint {id} not found"),
            )
        })?;
        endpoint.state = EndpointState::Disconnected;
        for route in self.routes.values_mut() {
            if route.source == id || route.destination == id {
                route.state = RouteState::Disconnected;
            }
        }
        Ok(())
    }

    fn validate_route(&self, route: &AudioRoute) -> Result<(), AudioError> {
        if route.source == route.destination {
            return Err(graph_error(
                ErrorCode::InvalidRoute,
                "A device cannot be connected to itself.",
                format!("route {} connects an endpoint to itself", route.id),
            ));
        }
        let source = self.endpoints.get(&route.source).ok_or_else(|| {
            graph_error(
                ErrorCode::EndpointNotFound,
                "The connection source is unavailable.",
                format!("route {} source {} not found", route.id, route.source),
            )
        })?;
        let destination = self.endpoints.get(&route.destination).ok_or_else(|| {
            graph_error(
                ErrorCode::EndpointNotFound,
                "The connection destination is unavailable.",
                format!(
                    "route {} destination {} not found",
                    route.id, route.destination
                ),
            )
        })?;
        if !source.endpoint_type.can_source() || !destination.endpoint_type.can_receive() {
            return Err(graph_error(
                ErrorCode::InvalidRoute,
                "Those devices cannot be connected.",
                format!(
                    "invalid direction from {:?} to {:?}",
                    source.endpoint_type, destination.endpoint_type
                ),
            ));
        }
        Ok(())
    }
}

fn graph_error(
    code: ErrorCode,
    user_message: impl Into<String>,
    technical_message: impl Into<String>,
) -> AudioError {
    AudioError::new(
        code,
        ErrorSeverity::Error,
        false,
        user_message,
        technical_message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelId, EndpointIdentity, EndpointType, GainDb, NormalizedBalance};

    fn endpoint(endpoint_type: EndpointType) -> AudioEndpoint {
        AudioEndpoint {
            id: EndpointId::new(),
            runtime_id: None,
            device_id: None,
            virtual_device_id: None,
            identity: EndpointIdentity::new("fake"),
            name: "test endpoint".into(),
            description: "test endpoint".into(),
            endpoint_type,
            state: EndpointState::Available,
            channel_count: 1,
            sample_rate: Some(48_000),
            is_default: false,
            channels: vec![ChannelId::new()],
            gain: match GainDb::new(0.0) {
                Ok(gain) => gain,
                Err(error) => panic!("valid test gain rejected: {error}"),
            },
            muted: false,
            balance: match NormalizedBalance::new(0.0) {
                Ok(balance) => balance,
                Err(error) => panic!("valid test balance rejected: {error}"),
            },
        }
    }

    fn populated_graph() -> (AudioGraph, AudioEndpoint, AudioEndpoint) {
        let source = endpoint(EndpointType::PhysicalInput);
        let destination = endpoint(EndpointType::PhysicalOutput);
        let mut graph = AudioGraph::default();
        assert!(graph.add_endpoint(source.clone()).is_ok());
        assert!(graph.add_endpoint(destination.clone()).is_ok());
        (graph, source, destination)
    }

    #[test]
    fn rejects_duplicate_source_destination_pairs() {
        let (mut graph, source, destination) = populated_graph();
        let first = AudioRoute {
            id: RouteId::new(),
            source: source.id,
            destination: destination.id,
            state: RouteState::Active,
        };
        let duplicate = AudioRoute {
            id: RouteId::new(),
            ..first.clone()
        };

        assert!(graph.add_route(first).is_ok());
        assert!(graph.add_route(duplicate).is_err());
    }

    #[test]
    fn validates_route_endpoints_and_direction() {
        let (mut graph, source, destination) = populated_graph();
        let missing = AudioRoute {
            id: RouteId::new(),
            source: EndpointId::new(),
            destination: destination.id,
            state: RouteState::Active,
        };
        let reversed = AudioRoute {
            id: RouteId::new(),
            source: destination.id,
            destination: source.id,
            state: RouteState::Active,
        };

        assert!(graph.add_route(missing).is_err());
        assert!(graph.add_route(reversed).is_err());
    }

    #[test]
    fn disconnect_preserves_endpoint_and_route() {
        let (mut graph, source, destination) = populated_graph();
        let route = AudioRoute {
            id: RouteId::new(),
            source: source.id,
            destination: destination.id,
            state: RouteState::Active,
        };
        let route_id = route.id;
        assert!(graph.add_route(route).is_ok());

        assert!(graph.disconnect_endpoint(source.id).is_ok());

        assert_eq!(
            graph.endpoints.get(&source.id).map(|item| item.state),
            Some(EndpointState::Disconnected)
        );
        assert_eq!(
            graph.routes.get(&route_id).map(|item| item.state),
            Some(RouteState::Disconnected)
        );
    }
}

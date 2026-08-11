use crossbeam_channel::{Receiver, Sender};

use super::{AudioBackend, BackendCommand, BackendEvent, BackendStatus};
use crate::domain::{
    AudioEndpoint, AudioError, AudioRoute, BackendCapabilities, EndpointId, EndpointState,
    ErrorCode, ErrorSeverity, RouteState,
};

#[derive(Clone, Debug)]
pub struct FakeBackend {
    endpoints: Vec<AudioEndpoint>,
    routes: Vec<AudioRoute>,
    /// Reported at startup; tests can simulate a platform without virtual
    /// device support.
    pub capabilities: BackendCapabilities,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self {
            endpoints: Vec::new(),
            routes: Vec::new(),
            // The fake implements CreateVirtual/DeleteVirtual, so it reports
            // support; tests override to simulate restricted platforms.
            capabilities: BackendCapabilities {
                virtual_devices: true,
            },
        }
    }
}

impl FakeBackend {
    pub fn new(endpoints: Vec<AudioEndpoint>) -> Self {
        Self {
            endpoints,
            capabilities: BackendCapabilities {
                virtual_devices: true,
            },
            routes: Vec::new(),
        }
    }

    fn send(events: &Sender<BackendEvent>, event: BackendEvent) -> bool {
        events.send(event).is_ok()
    }

    fn complete(events: &Sender<BackendEvent>, command: crate::domain::CommandId) -> bool {
        Self::send(
            events,
            BackendEvent::CommandCompleted {
                command_id: command,
            },
        )
    }

    fn endpoint_error(id: EndpointId) -> AudioError {
        AudioError::new(
            ErrorCode::EndpointNotFound,
            ErrorSeverity::Error,
            false,
            "That device is unavailable.",
            format!("fake backend endpoint {id} not found"),
        )
    }

    fn handle_command(&mut self, command: BackendCommand, events: &Sender<BackendEvent>) -> bool {
        match command {
            BackendCommand::SetVolume {
                command_id,
                endpoint_id,
                gain,
            } => self.update_endpoint(command_id, endpoint_id, events, |endpoint| {
                endpoint.gain = gain;
            }),
            BackendCommand::SetMute {
                command_id,
                endpoint_id,
                muted,
            } => self.update_endpoint(command_id, endpoint_id, events, |endpoint| {
                endpoint.muted = muted;
            }),
            BackendCommand::SetBalance {
                command_id,
                endpoint_id,
                balance,
            } => self.update_endpoint(command_id, endpoint_id, events, |endpoint| {
                endpoint.balance = balance;
            }),
            BackendCommand::Connect { command_id, route } => {
                self.routes.push(route.clone());
                Self::send(events, BackendEvent::RouteAdded { route })
                    && Self::complete(events, command_id)
            }
            BackendCommand::Disconnect {
                command_id,
                route_id,
            } => {
                let route = self.routes.iter_mut().find(|route| route.id == route_id);
                if let Some(route) = route {
                    route.state = RouteState::Disconnected;
                    let event = BackendEvent::RouteUpdated {
                        route: route.clone(),
                    };
                    Self::send(events, event) && Self::complete(events, command_id)
                } else {
                    let error = AudioError::new(
                        ErrorCode::RouteNotFound,
                        ErrorSeverity::Error,
                        false,
                        "The audio route no longer exists.",
                        format!("fake backend route {route_id} not found"),
                    );
                    Self::send(events, BackendEvent::CommandFailed { command_id, error })
                }
            }
            BackendCommand::CreateVirtual {
                command_id,
                virtual_device_id,
                mut endpoint,
            } => {
                endpoint.virtual_device_id = Some(virtual_device_id);
                endpoint.state = EndpointState::Available;
                self.endpoints.push((*endpoint).clone());
                Self::send(
                    events,
                    BackendEvent::EndpointAdded {
                        endpoint: *endpoint,
                    },
                ) && Self::complete(events, command_id)
            }
            BackendCommand::DeleteVirtual {
                command_id,
                virtual_device_id,
            } => {
                let removed: Vec<_> = self
                    .endpoints
                    .iter()
                    .filter(|endpoint| endpoint.virtual_device_id == Some(virtual_device_id))
                    .map(|endpoint| endpoint.id)
                    .collect();
                self.endpoints
                    .retain(|endpoint| endpoint.virtual_device_id != Some(virtual_device_id));
                for endpoint_id in removed {
                    if !Self::send(events, BackendEvent::EndpointRemoved { endpoint_id }) {
                        return false;
                    }
                }
                Self::complete(events, command_id)
            }
            BackendCommand::SetStreamTuning { command_id, .. } => {
                Self::complete(events, command_id)
            }
            BackendCommand::SetDelay { command_id, .. } => Self::complete(events, command_id),
            BackendCommand::SetEq { command_id, .. } => Self::complete(events, command_id),
            BackendCommand::SetChannelMode { command_id, .. } => Self::complete(events, command_id),
            BackendCommand::Shutdown { command_id } => {
                let _ = Self::send(
                    events,
                    BackendEvent::Status {
                        status: BackendStatus::Stopping,
                    },
                ) && Self::send(
                    events,
                    BackendEvent::Status {
                        status: BackendStatus::Stopped,
                    },
                ) && Self::send(events, BackendEvent::ShutdownComplete { command_id });
                false
            }
        }
    }

    fn update_endpoint(
        &mut self,
        command_id: crate::domain::CommandId,
        endpoint_id: EndpointId,
        events: &Sender<BackendEvent>,
        update: impl FnOnce(&mut AudioEndpoint),
    ) -> bool {
        let endpoint = self
            .endpoints
            .iter_mut()
            .find(|endpoint| endpoint.id == endpoint_id);
        if let Some(endpoint) = endpoint {
            update(endpoint);
            let event = BackendEvent::EndpointUpdated {
                endpoint: endpoint.clone(),
            };
            Self::send(events, event) && Self::complete(events, command_id)
        } else {
            Self::send(
                events,
                BackendEvent::CommandFailed {
                    command_id,
                    error: Self::endpoint_error(endpoint_id),
                },
            )
        }
    }
}

impl AudioBackend for FakeBackend {
    fn run(
        mut self: Box<Self>,
        commands: Receiver<BackendCommand>,
        events: Sender<BackendEvent>,
    ) -> Result<(), AudioError> {
        if !Self::send(
            &events,
            BackendEvent::Status {
                status: BackendStatus::Starting,
            },
        ) {
            return Ok(());
        }
        for endpoint in &self.endpoints {
            if !Self::send(
                &events,
                BackendEvent::EndpointAdded {
                    endpoint: endpoint.clone(),
                },
            ) {
                return Ok(());
            }
        }
        if !Self::send(
            &events,
            BackendEvent::Capabilities {
                capabilities: self.capabilities,
            },
        ) {
            return Ok(());
        }
        if !Self::send(
            &events,
            BackendEvent::Status {
                status: BackendStatus::Running,
            },
        ) {
            return Ok(());
        }

        while let Ok(command) = commands.recv() {
            if !self.handle_command(command, &events) {
                return Ok(());
            }
        }
        let _ = Self::send(
            &events,
            BackendEvent::Status {
                status: BackendStatus::Stopped,
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use crossbeam_channel::bounded;

    use super::*;
    use crate::domain::CommandId;

    #[test]
    fn fake_backend_starts_and_shuts_down_deterministically() {
        let (command_tx, command_rx) = bounded(4);
        let (event_tx, event_rx) = bounded(8);
        let backend: Box<dyn AudioBackend> = Box::new(FakeBackend::default());
        let worker = thread::spawn(move || backend.run(command_rx, event_tx));

        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(1)),
            Ok(BackendEvent::Status {
                status: BackendStatus::Starting
            })
        ));
        // Capabilities are reported before Running so the UI gates
        // affordances from the first snapshot.
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(1)),
            Ok(BackendEvent::Capabilities {
                capabilities: BackendCapabilities {
                    virtual_devices: true
                }
            })
        ));
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(1)),
            Ok(BackendEvent::Status {
                status: BackendStatus::Running
            })
        ));

        let command_id = CommandId::new();
        assert!(command_tx
            .send(BackendCommand::Shutdown { command_id })
            .is_ok());
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(1)),
            Ok(BackendEvent::Status {
                status: BackendStatus::Stopping
            })
        ));
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(1)),
            Ok(BackendEvent::Status {
                status: BackendStatus::Stopped
            })
        ));
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(1)),
            Ok(BackendEvent::ShutdownComplete { command_id: id }) if id == command_id
        ));
        assert!(matches!(worker.join(), Ok(Ok(()))));
    }
}

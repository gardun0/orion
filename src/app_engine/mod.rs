use std::any::Any;
use std::collections::HashMap;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender, TryRecvError};

use crate::backend::{AudioBackend, BackendCommand, BackendEvent, BackendStatus};
use crate::domain::{
    AudioError, AudioGraph, AudioRoute, CommandId, EndpointId, EngineCommand, EngineCommandKind,
    EngineEvent, EngineStatus, GraphDelta, MeterFrame, RouteId, RouteState,
};

const COMMAND_CAPACITY: usize = 64;
const EVENT_CAPACITY: usize = 256;

pub struct AudioEngine {
    commands: Sender<EngineCommand>,
    coordinator: Option<JoinHandle<Result<(), AudioError>>>,
}

pub struct EngineHandle {
    commands: Sender<EngineCommand>,
    events: Receiver<EngineEvent>,
}

impl AudioEngine {
    pub fn start<B: AudioBackend>(backend: B) -> Result<(Self, EngineHandle), AudioError> {
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        let (event_tx, event_rx) = unbounded();
        let coordinator = thread::Builder::new()
            .name("audio-coordinator".into())
            .spawn(move || run_coordinator(Box::new(backend), command_rx, event_tx))
            .map_err(|error| AudioError::thread_failure("coordinator", error.to_string()))?;

        let engine = Self {
            commands: command_tx.clone(),
            coordinator: Some(coordinator),
        };
        let handle = EngineHandle {
            commands: command_tx,
            events: event_rx,
        };
        Ok((engine, handle))
    }

    pub fn shutdown(mut self) -> Result<(), AudioError> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> Result<(), AudioError> {
        let Some(coordinator) = self.coordinator.take() else {
            return Ok(());
        };
        let _ = self.commands.send(EngineCommand::shutdown());
        match coordinator.join() {
            Ok(result) => result,
            Err(payload) => Err(AudioError::thread_failure(
                "coordinator",
                panic_detail(payload.as_ref()),
            )),
        }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        // Never block the UI thread on thread joins during window teardown:
        // PipeWire reaps our streams and virtual nodes when the connection
        // closes on process exit (they are created with object.linger=false).
        // The coordinator and backend exit on their own once the command
        // channel is disconnected; explicit shutdown() still joins them.
        let _ = self.commands.send(EngineCommand::shutdown());
        drop(self.coordinator.take());
    }
}

impl EngineHandle {
    pub fn send(&self, command: EngineCommand) -> Result<(), AudioError> {
        self.commands
            .send(command)
            .map_err(|_| AudioError::channel_closed("engine command"))
    }

    pub fn recv(&self) -> Result<EngineEvent, AudioError> {
        self.events
            .recv()
            .map_err(|_| AudioError::channel_closed("engine event"))
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<EngineEvent>, AudioError> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(None),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                Err(AudioError::channel_closed("engine event"))
            }
        }
    }

    pub fn try_recv(&self) -> Result<Option<EngineEvent>, AudioError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(AudioError::channel_closed("engine event")),
        }
    }
}

fn run_coordinator(
    backend: Box<dyn AudioBackend>,
    commands: Receiver<EngineCommand>,
    events: Sender<EngineEvent>,
) -> Result<(), AudioError> {
    let (backend_command_tx, backend_command_rx) = bounded(COMMAND_CAPACITY);
    let (backend_event_tx, backend_event_rx) = bounded(EVENT_CAPACITY);
    let backend_thread = thread::Builder::new()
        .name("audio-backend".into())
        .spawn(move || {
            let result = backend.run(backend_command_rx, backend_event_tx.clone());
            if let Err(error) = &result {
                let _ = backend_event_tx.send(BackendEvent::Error {
                    error: error.clone(),
                });
            }
            result
        })
        .map_err(|error| AudioError::thread_failure("backend", error.to_string()))?;

    let mut graph = AudioGraph::default();
    let mut shutdown_requested = false;
    let mut pending_meters: HashMap<EndpointId, MeterFrame> = HashMap::new();
    // Connect commands committed optimistically to the graph, awaiting the
    // backend outcome so a failure can roll the entry back.
    let mut pending_connect_routes: HashMap<CommandId, RouteId> = HashMap::new();

    loop {
        select! {
            recv(commands) -> command => {
                match command {
                    Ok(command) => {
                        if handle_engine_command(
                            command,
                            &mut graph,
                            &backend_command_tx,
                            &events,
                            &mut pending_connect_routes,
                        )? {
                            shutdown_requested = true;
                        }
                    }
                    Err(_) if !shutdown_requested => {
                        shutdown_requested = true;
                        if backend_command_tx.send(BackendCommand::Shutdown {
                            command_id: CommandId::new(),
                        }).is_err() {
                            break;
                        }
                    }
                    Err(_) => {}
                }
            }
            recv(backend_event_rx) -> event => {
                match event {
                    Ok(event) => {
                        // Roll back optimistic route entries when the backend
                        // rejects the command that created them; on success
                        // the route is confirmed by its RouteAdded event.
                        match &event {
                            BackendEvent::CommandFailed { command_id, .. } => {
                                if let Some(route_id) = pending_connect_routes.remove(command_id)
                                {
                                    graph.routes.remove(&route_id);
                                }
                            }
                            BackendEvent::CommandCompleted { command_id } => {
                                pending_connect_routes.remove(command_id);
                            }
                            BackendEvent::RouteAdded { route } | BackendEvent::RouteUpdated { route } => {
                                pending_connect_routes.retain(|_, route_id| *route_id != route.id);
                            }
                            _ => {}
                        }
                        let (outgoing, shutdown_complete) = reduce_backend_event(&mut graph, event);
                        for event in outgoing {
                            publish_event(&events, event, &mut pending_meters);
                        }
                        if shutdown_complete {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        flush_pending_meters(&events, &mut pending_meters);
    }

    drop(backend_command_tx);
    match backend_thread.join() {
        Ok(result) => result,
        Err(payload) => Err(AudioError::thread_failure(
            "backend",
            panic_detail(payload.as_ref()),
        )),
    }
}

fn handle_engine_command(
    command: EngineCommand,
    graph: &mut AudioGraph,
    backend_commands: &Sender<BackendCommand>,
    events: &Sender<EngineEvent>,
    pending_connect_routes: &mut HashMap<CommandId, RouteId>,
) -> Result<bool, AudioError> {
    let command_id = command.id;
    let command_route = match &command.kind {
        EngineCommandKind::Connect { route_id, .. } => Some(*route_id),
        _ => None,
    };
    let backend_command = match into_backend_command(command, graph) {
        Ok(command) => command,
        Err(error) => {
            publish(
                events,
                EngineEvent::Error {
                    command_id: Some(command_id),
                    error,
                },
            );
            return Ok(false);
        }
    };
    let is_shutdown = matches!(backend_command, BackendCommand::Shutdown { .. });
    if let Some(route_id) = command_route {
        pending_connect_routes.insert(command_id, route_id);
    }
    backend_commands
        .send(backend_command)
        .map_err(|_| AudioError::channel_closed("backend command"))?;
    Ok(is_shutdown)
}

fn into_backend_command(
    command: EngineCommand,
    graph: &mut AudioGraph,
) -> Result<BackendCommand, AudioError> {
    let command_id = command.id;
    match command.kind {
        EngineCommandKind::SetVolume { endpoint_id, gain } => Ok(BackendCommand::SetVolume {
            command_id,
            endpoint_id,
            gain,
        }),
        EngineCommandKind::SetMute { endpoint_id, muted } => Ok(BackendCommand::SetMute {
            command_id,
            endpoint_id,
            muted,
        }),
        EngineCommandKind::SetBalance {
            endpoint_id,
            balance,
        } => Ok(BackendCommand::SetBalance {
            command_id,
            endpoint_id,
            balance,
        }),
        EngineCommandKind::Connect {
            route_id,
            source,
            destination,
        } => {
            let route = AudioRoute {
                id: route_id,
                source,
                destination,
                state: RouteState::Connecting,
            };
            // Commit optimistically: validation sees in-flight routes, and a
            // same-pair Disconnect+Connect back to back no longer trips the
            // duplicate check. A backend failure rolls the entry back.
            graph.add_route(route.clone())?;
            Ok(BackendCommand::Connect { command_id, route })
        }
        EngineCommandKind::Disconnect { route_id } => {
            // Forget it now so a reconnect of the same pair validates.
            graph.routes.remove(&route_id);
            Ok(BackendCommand::Disconnect {
                command_id,
                route_id,
            })
        }
        EngineCommandKind::CreateVirtual {
            virtual_device_id,
            endpoint,
        } => Ok(BackendCommand::CreateVirtual {
            command_id,
            virtual_device_id,
            endpoint,
        }),
        EngineCommandKind::DeleteVirtual { virtual_device_id } => {
            Ok(BackendCommand::DeleteVirtual {
                command_id,
                virtual_device_id,
            })
        }
        EngineCommandKind::Shutdown => Ok(BackendCommand::Shutdown { command_id }),
        EngineCommandKind::SetStreamTuning {
            stream_rate,
            buffer_frames,
        } => Ok(BackendCommand::SetStreamTuning {
            command_id,
            stream_rate,
            buffer_frames,
        }),
        EngineCommandKind::SetDelay {
            endpoint_id,
            delay_ms,
        } => Ok(BackendCommand::SetDelay {
            command_id,
            endpoint_id,
            delay_ms,
        }),
        EngineCommandKind::SetEq {
            endpoint_id,
            low_db,
            mid_db,
            high_db,
        } => Ok(BackendCommand::SetEq {
            command_id,
            endpoint_id,
            low_db,
            mid_db,
            high_db,
        }),
        EngineCommandKind::SetChannelMode { endpoint_id, mode } => {
            Ok(BackendCommand::SetChannelMode {
                command_id,
                endpoint_id,
                mode,
            })
        }
    }
}

fn reduce_backend_event(graph: &mut AudioGraph, event: BackendEvent) -> (Vec<EngineEvent>, bool) {
    let mut outgoing = Vec::new();
    let mut shutdown_complete = false;
    match event {
        BackendEvent::Status { status } => {
            outgoing.push(EngineEvent::Status {
                status: map_status(status),
            });
            if status == BackendStatus::Running {
                outgoing.push(EngineEvent::Snapshot {
                    graph: graph.clone(),
                });
            }
        }
        BackendEvent::Capabilities { capabilities } => {
            outgoing.push(EngineEvent::Capabilities { capabilities });
        }
        BackendEvent::EndpointAdded { endpoint } => {
            let delta = if graph.endpoints.contains_key(&endpoint.id) {
                GraphDelta::EndpointUpdated {
                    endpoint: endpoint.clone(),
                }
            } else {
                GraphDelta::EndpointAdded {
                    endpoint: endpoint.clone(),
                }
            };
            graph.upsert_endpoint(endpoint);
            outgoing.push(EngineEvent::Delta {
                delta: Box::new(delta),
            });
        }
        BackendEvent::EndpointUpdated { endpoint } => {
            graph.upsert_endpoint(endpoint.clone());
            outgoing.push(EngineEvent::Delta {
                delta: Box::new(GraphDelta::EndpointUpdated { endpoint }),
            });
        }
        BackendEvent::EndpointRemoved { endpoint_id } => {
            let is_managed_virtual = graph
                .endpoints
                .get(&endpoint_id)
                .is_some_and(|endpoint| endpoint.virtual_device_id.is_some());
            if is_managed_virtual {
                graph.endpoints.remove(&endpoint_id);
                graph.routes.retain(|_, route| {
                    route.source != endpoint_id && route.destination != endpoint_id
                });
                outgoing.push(EngineEvent::Delta {
                    delta: Box::new(GraphDelta::EndpointRemoved { endpoint_id }),
                });
            } else if let Err(error) = graph.disconnect_endpoint(endpoint_id) {
                outgoing.push(EngineEvent::Error {
                    command_id: None,
                    error,
                });
            } else {
                if let Some(endpoint) = graph.endpoints.get(&endpoint_id).cloned() {
                    outgoing.push(EngineEvent::Delta {
                        delta: Box::new(GraphDelta::EndpointUpdated { endpoint }),
                    });
                }
                for route in graph.routes.values() {
                    if route.source == endpoint_id || route.destination == endpoint_id {
                        outgoing.push(EngineEvent::Delta {
                            delta: Box::new(GraphDelta::RouteUpdated {
                                route: route.clone(),
                            }),
                        });
                    }
                }
            }
        }
        BackendEvent::RouteAdded { route } | BackendEvent::RouteUpdated { route } => {
            let is_new = !graph.routes.contains_key(&route.id);
            match graph.upsert_route(route.clone()) {
                Ok(()) if is_new => outgoing.push(EngineEvent::Delta {
                    delta: Box::new(GraphDelta::RouteAdded { route }),
                }),
                Ok(()) => outgoing.push(EngineEvent::Delta {
                    delta: Box::new(GraphDelta::RouteUpdated { route }),
                }),
                Err(error) => outgoing.push(EngineEvent::Error {
                    command_id: None,
                    error,
                }),
            }
        }
        BackendEvent::RouteRemoved { route_id } => {
            graph.routes.remove(&route_id);
            outgoing.push(EngineEvent::Delta {
                delta: Box::new(GraphDelta::RouteRemoved { route_id }),
            });
        }
        BackendEvent::Meter { frame } => outgoing.push(EngineEvent::Meter { frame }),
        BackendEvent::AudioSettings {
            rate,
            quantum,
            min_quantum,
            max_quantum,
        } => outgoing.push(EngineEvent::AudioSettings {
            rate,
            quantum,
            min_quantum,
            max_quantum,
        }),
        BackendEvent::CommandCompleted { command_id } => {
            outgoing.push(EngineEvent::CommandCompleted { command_id });
        }
        BackendEvent::CommandFailed { command_id, error } => {
            outgoing.push(EngineEvent::Error {
                command_id: Some(command_id),
                error,
            });
        }
        BackendEvent::Error { error } => outgoing.push(EngineEvent::Error {
            command_id: None,
            error,
        }),
        BackendEvent::ShutdownComplete { command_id } => {
            outgoing.push(EngineEvent::CommandCompleted { command_id });
            shutdown_complete = true;
        }
    }
    (outgoing, shutdown_complete)
}

fn map_status(status: BackendStatus) -> EngineStatus {
    match status {
        BackendStatus::Starting => EngineStatus::Starting,
        BackendStatus::Running => EngineStatus::Running,
        BackendStatus::Stopping => EngineStatus::Stopping,
        BackendStatus::Stopped => EngineStatus::Stopped,
    }
}

fn publish(events: &Sender<EngineEvent>, event: EngineEvent) {
    let _ = events.send(event);
}

/// Meter frames are coalesced per endpoint under backpressure: the newest
/// frame for an endpoint always wins, and a full queue never loses it.
fn publish_event(
    events: &Sender<EngineEvent>,
    event: EngineEvent,
    pending_meters: &mut HashMap<EndpointId, MeterFrame>,
) {
    if let EngineEvent::Meter { frame } = event {
        if events.len() >= EVENT_CAPACITY {
            pending_meters.insert(frame.endpoint_id, frame);
        } else {
            let _ = events.send(EngineEvent::Meter { frame });
        }
    } else {
        publish(events, event);
    }
}

fn flush_pending_meters(
    events: &Sender<EngineEvent>,
    pending_meters: &mut HashMap<EndpointId, MeterFrame>,
) {
    if pending_meters.is_empty() || events.len() >= EVENT_CAPACITY {
        return;
    }
    let frames: Vec<MeterFrame> = pending_meters.drain().map(|(_, frame)| frame).collect();
    for frame in frames {
        if events.len() >= EVENT_CAPACITY {
            pending_meters.insert(frame.endpoint_id, frame);
        } else {
            let _ = events.send(EngineEvent::Meter { frame });
        }
    }
}

fn panic_detail(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a same-pair Disconnect followed immediately by Connect
    /// must not be rejected as a duplicate (the graph used to learn about
    /// removals only via async route events).
    #[test]
    fn same_pair_disconnect_then_connect_is_not_rejected() {
        let mut graph = AudioGraph::default();
        let source = endpoint();
        let mut destination = endpoint();
        destination.endpoint_type = EndpointType::PhysicalOutput;
        graph.upsert_endpoint(source.clone());
        graph.upsert_endpoint(destination.clone());
        let (backend_tx, _backend_rx) = bounded(COMMAND_CAPACITY);
        let (events, _events_rx) = unbounded();
        let mut pending = HashMap::new();

        let connect = |graph: &mut AudioGraph, pending: &mut HashMap<CommandId, RouteId>| {
            handle_engine_command(
                EngineCommand::new(EngineCommandKind::Connect {
                    route_id: RouteId::new(),
                    source: source.id,
                    destination: destination.id,
                }),
                graph,
                &backend_tx,
                &events,
                pending,
            )
            .expect("connect accepted")
        };

        connect(&mut graph, &mut pending);
        // Disconnect, then immediately reconnect the same pair.
        let route_id = graph.routes.keys().next().copied().expect("route present");
        handle_engine_command(
            EngineCommand::new(EngineCommandKind::Disconnect { route_id }),
            &mut graph,
            &backend_tx,
            &events,
            &mut pending,
        )
        .expect("disconnect accepted");
        connect(&mut graph, &mut pending);
        assert_eq!(
            graph.routes.len(),
            1,
            "exactly one route for the pair after the churn"
        );
    }

    use crate::backend::FakeBackend;
    use crate::domain::{
        AudioEndpoint, ChannelId, EndpointId, EndpointIdentity, EndpointState, EndpointType,
        GainDb, NormalizedBalance,
    };

    fn endpoint() -> AudioEndpoint {
        AudioEndpoint {
            id: EndpointId::new(),
            runtime_id: None,
            device_id: None,
            virtual_device_id: None,
            identity: EndpointIdentity::new("fake"),
            name: "fake input".into(),
            description: "fake input".into(),
            endpoint_type: EndpointType::PhysicalInput,
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

    #[test]
    fn engine_starts_fake_backend_and_joins_on_shutdown() {
        let started = AudioEngine::start(FakeBackend::default());
        let (engine, handle) = match started {
            Ok(parts) => parts,
            Err(error) => panic!("failed to start engine: {error}"),
        };

        let mut running = false;
        for _ in 0..4 {
            let event = handle.recv_timeout(Duration::from_secs(1));
            if matches!(
                event,
                Ok(Some(EngineEvent::Status {
                    status: EngineStatus::Running
                }))
            ) {
                running = true;
                break;
            }
        }

        assert!(running);
        assert!(engine.shutdown().is_ok());
    }

    #[test]
    fn backend_add_and_remove_preserve_disconnected_endpoint() {
        let endpoint = endpoint();
        let endpoint_id = endpoint.id;
        let mut graph = AudioGraph::default();

        let _ = reduce_backend_event(
            &mut graph,
            BackendEvent::EndpointAdded {
                endpoint: endpoint.clone(),
            },
        );
        let _ = reduce_backend_event(&mut graph, BackendEvent::EndpointRemoved { endpoint_id });

        assert_eq!(
            graph.endpoints.get(&endpoint_id).map(|item| item.state),
            Some(EndpointState::Disconnected)
        );
    }
}

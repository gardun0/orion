use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    error::Error,
    ffi::{c_void, CString},
    ptr,
    rc::Rc,
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use pipewire::{self as pw, loop_::Timeout, types::ObjectType};
use uuid::Uuid;

use super::streams::{
    CaptureRuntime, PlaybackRuntime, STREAM_CONNECTED, STREAM_CONNECTING, STREAM_DISCONNECTED,
    STREAM_FAILED,
};
use super::{AudioBackend, BackendCommand, BackendEvent, BackendStatus};
use crate::domain::{
    AudioEndpoint, AudioError, AudioRoute, ChannelId, DeviceId, EndpointId, EndpointIdentity,
    EndpointState, EndpointType, ErrorCode, ErrorSeverity, GainDb, MeterFrame, MeterLevel,
    NormalizedBalance, RouteId, RouteState, VirtualDeviceId,
};
use crate::realtime::{ControlHub, RouteLink, TARGET_QUANTA};

const ORION_ID_NAMESPACE: Uuid = Uuid::from_u128(0x56ef_da46_0a0a_4f64_a31e_3f76_74fc_b476);
const DEFAULT_METADATA_NAME: &str = "default";
const SETTINGS_METADATA_NAME: &str = "settings";
const DEFAULT_AUDIO_SOURCE: &str = "default.audio.source";
const DEFAULT_AUDIO_SINK: &str = "default.audio.sink";
const ORION_VIRTUAL_ID: &str = "orion.virtual-device-id";
const ORION_VIRTUAL_ROLE: &str = "orion.virtual-device-role";
const ORION_APPLICATION_ID: &str = "io.github.gardun0.orion";
const METER_INTERVAL: Duration = Duration::from_millis(33);
const VIRTUAL_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(500);
const CONNECT_RETRY_LIMIT: u8 = 10;
/// Orion always exposes at least one virtual device per direction; users can
/// add more from the Virtual I/O page. The second stable IDs stay reserved for
/// users upgrading from builds that seeded two by default.
const DEFAULT_VIRTUAL_INPUT_IDS: [u128; 1] = [0xa1e9_2d80_4a87_4a77_9a52_1cb5_3fce_0001];
const DEFAULT_VIRTUAL_OUTPUT_IDS: [u128; 1] = [0xb2fa_3e91_5b98_4b88_ab63_2dc6_40df_0001];

#[derive(Default)]
pub struct PipeWireBackend;

#[derive(Default)]
struct DefaultEndpoints {
    source: Option<String>,
    sink: Option<String>,
}

struct NodeBinding {
    _node: pw::node::Node,
    _listener: pw::node::NodeListener,
}

struct MetadataBinding {
    _metadata: pw::metadata::Metadata,
    _listener: pw::metadata::MetadataListener,
}

enum ManagedVirtualResource {
    Node(pw::node::Node),
    Loopback(*mut c_void),
}

struct ManagedVirtualNode {
    resource: ManagedVirtualResource,
    endpoint_type: EndpointType,
}

#[derive(Debug)]
struct PortRecord {
    _node_id: Option<u32>,
    _direction: Option<String>,
    _channel: Option<String>,
    _monitor: bool,
}

#[derive(Debug)]
struct LinkRecord {
    _output_node: Option<u32>,
    _output_port: Option<u32>,
    _input_node: Option<u32>,
    _input_port: Option<u32>,
}

#[derive(Default)]
struct RegistryState {
    endpoints: HashMap<u32, AudioEndpoint>,
    node_bindings: HashMap<u32, NodeBinding>,
    metadata_bindings: HashMap<u32, MetadataBinding>,
    devices: HashMap<u32, HashMap<String, String>>,
    ports: HashMap<u32, PortRecord>,
    links: HashMap<u32, LinkRecord>,
    factories: HashMap<u32, String>,
    managed_virtuals: HashMap<VirtualDeviceId, ManagedVirtualNode>,
    virtual_ingresses: HashMap<VirtualDeviceId, u32>,
    defaults: DefaultEndpoints,
    graph_clock: GraphClock,
}

/// PipeWire graph clock detected from the `settings` metadata.
#[derive(Default)]
struct GraphClock {
    rate: Option<u32>,
    quantum: Option<u32>,
    min_quantum: Option<u32>,
    max_quantum: Option<u32>,
}

impl GraphClock {
    fn complete(&self) -> Option<(u32, u32, u32, u32)> {
        Some((
            self.rate?,
            self.quantum?,
            self.min_quantum.unwrap_or(32),
            self.max_quantum.unwrap_or(8_192),
        ))
    }
}

/// Endpoint-centric stream topology: one capture runtime per source
/// endpoint, one playback (bus) runtime per destination endpoint, and routes
/// as engine links between them. Replacing a route's ring only requires
/// publishing new plans — streams survive topology changes.
#[derive(Default)]
struct Runtimes<'core> {
    captures: HashMap<EndpointId, CaptureRuntime<'core>>,
    buses: HashMap<EndpointId, PlaybackRuntime<'core>>,
    routes: HashMap<RouteId, AudioRoute>,
}

impl<'core> Runtimes<'core> {
    fn disconnect_all(&mut self) {
        for runtime in self.captures.values() {
            let _ = runtime.disconnect();
        }
        for runtime in self.buses.values() {
            let _ = runtime.disconnect();
        }
        self.captures.clear();
        self.buses.clear();
        self.routes.clear();
    }

    /// Route liveness from both endpoint stream states (Connecting until
    /// both sides stream, Failed/Disconnected if either side breaks).
    fn route_state(&self, route: &AudioRoute) -> RouteState {
        let capture = self
            .captures
            .get(&route.source)
            .map_or(STREAM_CONNECTING, |runtime| runtime.stream_state());
        let bus = self
            .buses
            .get(&route.destination)
            .map_or(STREAM_CONNECTING, |runtime| runtime.stream_state());
        if capture == STREAM_FAILED || bus == STREAM_FAILED {
            RouteState::Failed
        } else if capture == STREAM_DISCONNECTED || bus == STREAM_DISCONNECTED {
            RouteState::Disconnected
        } else if capture == STREAM_CONNECTED && bus == STREAM_CONNECTED {
            RouteState::Active
        } else {
            RouteState::Connecting
        }
    }

    fn meter_frames(&self, state: &Rc<RefCell<RegistryState>>) -> Vec<MeterFrame> {
        let state = state.borrow();
        let mut frames = Vec::with_capacity(self.captures.len() + self.buses.len());
        for runtime in self.captures.values() {
            if let Some(frame) = meter_frame_for(&state, runtime.endpoint_id(), runtime.meter()) {
                frames.push(frame);
            }
        }
        for runtime in self.buses.values() {
            if let Some(frame) = meter_frame_for(&state, runtime.endpoint_id(), runtime.meter()) {
                frames.push(frame);
            }
        }
        frames
    }

    fn collect_garbage(&mut self) {
        for runtime in self.captures.values_mut() {
            runtime.collect_garbage();
        }
        for runtime in self.buses.values_mut() {
            runtime.collect_garbage();
        }
    }
}

/// Build an endpoint meter frame from a runtime's meter, using the
/// endpoint's channel identities. Returns None when the endpoint vanished.
fn meter_frame_for(
    state: &RegistryState,
    endpoint_id: EndpointId,
    meter: &std::sync::Arc<crate::realtime::RouteMeter>,
) -> Option<MeterFrame> {
    let endpoint = state
        .endpoints
        .values()
        .find(|endpoint| endpoint.id == endpoint_id)?;
    let levels = endpoint
        .channels
        .iter()
        .copied()
        .zip(meter.levels())
        .map(|(channel, peak)| {
            let level =
                MeterLevel::new(peak.clamp(MeterLevel::MIN, MeterLevel::MAX)).unwrap_or_default();
            (channel, level)
        })
        .collect();
    Some(MeterFrame {
        endpoint_id,
        sequence: meter.next_sequence(),
        levels,
    })
}

#[link(name = "pipewire-0.3")]
extern "C" {
    fn pw_context_load_module(
        context: *mut pw::sys::pw_context,
        name: *const std::ffi::c_char,
        args: *const std::ffi::c_char,
        properties: *mut pw::sys::pw_properties,
    ) -> *mut c_void;
    fn pw_impl_module_destroy(module: *mut c_void);
}

impl AudioBackend for PipeWireBackend {
    fn run(
        self: Box<Self>,
        commands: Receiver<BackendCommand>,
        events: Sender<BackendEvent>,
    ) -> Result<(), AudioError> {
        let _ = self;
        send_event(
            &events,
            BackendEvent::Status {
                status: BackendStatus::Starting,
            },
        );

        run_pipewire(commands, events.clone()).map_err(|error| {
            AudioError::new(
                ErrorCode::BackendUnavailable,
                ErrorSeverity::Error,
                true,
                "Orion could not connect to PipeWire.",
                error.to_string(),
            )
        })
    }
}

fn run_pipewire(
    commands: Receiver<BackendCommand>,
    events: Sender<BackendEvent>,
) -> Result<(), Box<dyn Error>> {
    let main_loop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let state = Rc::new(RefCell::new(RegistryState::default()));
    let should_stop = Rc::new(Cell::new(false));
    let initial_sync = Rc::new(Cell::new(None));

    let done_events = events.clone();
    let done_sync = initial_sync.clone();
    let error_events = events.clone();
    let error_stop = should_stop.clone();
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && done_sync.get() == Some(seq) {
                done_sync.set(None);
                send_event(
                    &done_events,
                    BackendEvent::Status {
                        status: BackendStatus::Running,
                    },
                );
            }
        })
        .error(move |id, _seq, result, message| {
            let fatal = id == pw::core::PW_ID_CORE;
            send_event(
                &error_events,
                BackendEvent::Error {
                    error: AudioError::new(
                        ErrorCode::BackendUnavailable,
                        if fatal {
                            ErrorSeverity::Error
                        } else {
                            ErrorSeverity::Warning
                        },
                        true,
                        if fatal {
                            "The connection to PipeWire was lost."
                        } else {
                            "PipeWire reported an audio object error."
                        },
                        format!("PipeWire object {id} error ({result}): {message}"),
                    ),
                },
            );
            if fatal {
                error_stop.set(true);
            }
        })
        .register();

    let registry_for_add = registry.clone();
    let state_for_add = state.clone();
    let events_for_add = events.clone();
    let state_for_remove = state.clone();
    let events_for_remove = events.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            handle_global(&registry_for_add, global, &state_for_add, &events_for_add);
        })
        .global_remove(move |object_id| {
            handle_global_remove(object_id, &state_for_remove, &events_for_remove);
        })
        .register();

    create_default_virtuals(&context, &core, &state, &events);

    // PipeWire manages virtual devices natively; report capabilities before
    // the Running snapshot so the UI gates affordances correctly from the
    // start.
    send_event(
        &events,
        BackendEvent::Capabilities {
            capabilities: crate::domain::BackendCapabilities {
                virtual_devices: true,
            },
        },
    );

    initial_sync.set(Some(core.sync(0)?));
    let mut runtimes = Runtimes::default();
    let mut pending_connects = Vec::<PendingConnect>::new();
    // Stall-rebuild budget per endpoint: at most a few rebuilds, then a
    // cooldown (a genuinely asleep device would otherwise rebuild forever).
    let mut rebuild_budget = HashMap::<EndpointId, (u8, Instant)>::new();
    let hub = std::sync::Arc::new(ControlHub::default());
    let mut last_meter = Instant::now();
    let mut last_virtual_reconcile = Instant::now();

    while !should_stop.get() {
        main_loop
            .loop_()
            .iterate(Timeout::Finite(Duration::from_millis(20)));

        loop {
            match commands.try_recv() {
                Ok(command) => {
                    if handle_command(
                        command,
                        &context,
                        &core,
                        &state,
                        &mut runtimes,
                        &mut pending_connects,
                        &hub,
                        &events,
                    ) {
                        should_stop.set(true);
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    should_stop.set(true);
                    break;
                }
            }
        }

        retry_pending_connects(
            &core,
            &state,
            &mut runtimes,
            &mut pending_connects,
            &hub,
            &events,
        );
        refresh_route_states(&mut runtimes, &events);
        rebuild_stalled_streams(
            &core,
            &state,
            &mut runtimes,
            &hub,
            &events,
            &mut rebuild_budget,
        );
        prune_unavailable_routes(&mut runtimes, &state, &events);
        runtimes.collect_garbage();
        if last_virtual_reconcile.elapsed() >= VIRTUAL_RECONCILE_INTERVAL {
            reconcile_default_virtuals(&context, &core, &state, &events);
            last_virtual_reconcile = Instant::now();
        }
        if last_meter.elapsed() >= METER_INTERVAL {
            // One runtime per endpoint per role: each endpoint's level is
            // metered exactly once, where its audio actually flows.
            for frame in runtimes.meter_frames(&state) {
                let _ = events.try_send(BackendEvent::Meter { frame });
            }
            last_meter = Instant::now();
        }
    }

    runtimes.disconnect_all();
    destroy_all_virtuals(&core, &state);

    send_event(
        &events,
        BackendEvent::Status {
            status: BackendStatus::Stopped,
        },
    );
    Ok(())
}

fn handle_global(
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
    state: &Rc<RefCell<RegistryState>>,
    events: &Sender<BackendEvent>,
) {
    match global.type_ {
        ObjectType::Node => {
            if let Some(props) = global.props {
                let ingress_id = props
                    .get(ORION_VIRTUAL_ID)
                    .and_then(|value| Uuid::parse_str(value).ok())
                    .map(VirtualDeviceId::from_uuid)
                    .or_else(|| {
                        props.get(*pw::keys::NODE_NAME).and_then(|name| {
                            virtual_id_from_name(name, "orion.virtual-output-ingress.")
                        })
                    });
                if props.get(ORION_VIRTUAL_ROLE) == Some("output-ingress") || ingress_id.is_some() {
                    if let Some(virtual_device_id) = ingress_id {
                        state
                            .borrow_mut()
                            .virtual_ingresses
                            .insert(virtual_device_id, global.id);
                    }
                    return;
                }
            }
            handle_node(registry, global, state, events);
        }
        ObjectType::Device => {
            if let Some(props) = global.props {
                state
                    .borrow_mut()
                    .devices
                    .insert(global.id, owned_properties(props));
            }
        }
        ObjectType::Port => {
            if let Some(props) = global.props {
                state.borrow_mut().ports.insert(
                    global.id,
                    PortRecord {
                        _node_id: parse_property(props.get(*pw::keys::NODE_ID)),
                        _direction: props.get(*pw::keys::PORT_DIRECTION).map(str::to_owned),
                        _channel: props.get(*pw::keys::AUDIO_CHANNEL).map(str::to_owned),
                        _monitor: property_is_true(props.get(*pw::keys::PORT_MONITOR)),
                    },
                );
            }
        }
        ObjectType::Link => {
            if let Some(props) = global.props {
                state.borrow_mut().links.insert(
                    global.id,
                    LinkRecord {
                        _output_node: parse_property(props.get(*pw::keys::LINK_OUTPUT_NODE)),
                        _output_port: parse_property(props.get(*pw::keys::LINK_OUTPUT_PORT)),
                        _input_node: parse_property(props.get(*pw::keys::LINK_INPUT_NODE)),
                        _input_port: parse_property(props.get(*pw::keys::LINK_INPUT_PORT)),
                    },
                );
            }
        }
        ObjectType::Factory => {
            if let Some(name) = global
                .props
                .and_then(|props| props.get(*pw::keys::FACTORY_NAME))
            {
                state
                    .borrow_mut()
                    .factories
                    .insert(global.id, name.to_owned());
            }
        }
        ObjectType::Metadata => handle_metadata(registry, global, state, events),
        _ => {}
    }
}

fn handle_node(
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
    state: &Rc<RefCell<RegistryState>>,
    events: &Sender<BackendEvent>,
) {
    let Some(mut endpoint) = endpoint_from_global(global) else {
        return;
    };
    apply_default(&mut endpoint, &state.borrow().defaults);
    let endpoint_id = endpoint.id;
    state
        .borrow_mut()
        .endpoints
        .insert(global.id, endpoint.clone());
    send_event(events, BackendEvent::EndpointAdded { endpoint });

    let Ok(node) = registry.bind::<pw::node::Node, _>(global) else {
        return;
    };
    let object_id = global.id;
    let listener_state = state.clone();
    let listener_events = events.clone();
    let listener = node
        .add_listener_local()
        .info(move |info| {
            let mut state = listener_state.borrow_mut();
            let Some(endpoint) = state.endpoints.get_mut(&object_id) else {
                return;
            };
            endpoint.state = endpoint_state(info.state());
            if let Some(props) = info.props() {
                update_endpoint_properties(endpoint, props);
            }
            debug_assert_eq!(endpoint.id, endpoint_id);
            send_event(
                &listener_events,
                BackendEvent::EndpointUpdated {
                    endpoint: endpoint.clone(),
                },
            );
        })
        .register();
    state.borrow_mut().node_bindings.insert(
        global.id,
        NodeBinding {
            _node: node,
            _listener: listener,
        },
    );
}

fn handle_metadata(
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
    state: &Rc<RefCell<RegistryState>>,
    events: &Sender<BackendEvent>,
) {
    let name = global.props.and_then(|props| props.get("metadata.name"));
    if name == Some(SETTINGS_METADATA_NAME) {
        handle_settings_metadata(registry, global, state, events);
        return;
    }
    if name != Some(DEFAULT_METADATA_NAME) {
        return;
    }
    let Ok(metadata) = registry.bind::<pw::metadata::Metadata, _>(global) else {
        return;
    };
    let metadata_state = state.clone();
    let metadata_events = events.clone();
    let listener = metadata
        .add_listener_local()
        .property(move |_subject, key, _type, value| {
            let mut state = metadata_state.borrow_mut();
            match key {
                Some(DEFAULT_AUDIO_SOURCE) => {
                    state.defaults.source = default_node_name(value);
                }
                Some(DEFAULT_AUDIO_SINK) => {
                    state.defaults.sink = default_node_name(value);
                }
                None => {
                    state.defaults = DefaultEndpoints::default();
                }
                _ => return 0,
            }
            refresh_defaults(&mut state, &metadata_events);
            0
        })
        .register();
    state.borrow_mut().metadata_bindings.insert(
        global.id,
        MetadataBinding {
            _metadata: metadata,
            _listener: listener,
        },
    );
}

/// Bind the PipeWire `settings` metadata and publish the graph clock
/// (rate/quantum) so Orion follows the system instead of asking the user.
fn handle_settings_metadata(
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
    state: &Rc<RefCell<RegistryState>>,
    events: &Sender<BackendEvent>,
) {
    let Ok(metadata) = registry.bind::<pw::metadata::Metadata, _>(global) else {
        return;
    };
    let settings_state = state.clone();
    let settings_events = events.clone();
    let listener = metadata
        .add_listener_local()
        .property(move |_subject, key, _type, value| {
            let Some(key) = key else {
                return 0;
            };
            let mut state = settings_state.borrow_mut();
            let settings = &mut state.graph_clock;
            match key {
                "clock.rate" => settings.rate = parse_property(value),
                "clock.quantum" => settings.quantum = parse_property(value),
                "clock.min-quantum" => settings.min_quantum = parse_property(value),
                "clock.max-quantum" => settings.max_quantum = parse_property(value),
                _ => return 0,
            }
            if let Some(clock) = settings.complete() {
                send_event(
                    &settings_events,
                    BackendEvent::AudioSettings {
                        rate: clock.0,
                        quantum: clock.1,
                        min_quantum: clock.2,
                        max_quantum: clock.3,
                    },
                );
            }
            0
        })
        .register();
    state.borrow_mut().metadata_bindings.insert(
        global.id,
        MetadataBinding {
            _metadata: metadata,
            _listener: listener,
        },
    );
}

fn handle_global_remove(
    object_id: u32,
    state: &Rc<RefCell<RegistryState>>,
    events: &Sender<BackendEvent>,
) {
    let (removed_endpoint, stale_managed) = {
        let mut state = state.borrow_mut();
        state.node_bindings.remove(&object_id);
        state.devices.remove(&object_id);
        state.ports.remove(&object_id);
        state.links.remove(&object_id);
        state.factories.remove(&object_id);
        let removed_ingress = state
            .virtual_ingresses
            .iter()
            .find_map(|(virtual_id, runtime_id)| (*runtime_id == object_id).then_some(*virtual_id));
        if let Some(virtual_id) = removed_ingress {
            state.virtual_ingresses.remove(&virtual_id);
        }
        if state.metadata_bindings.remove(&object_id).is_some() {
            state.defaults = DefaultEndpoints::default();
            refresh_defaults(&mut state, events);
        }
        let removed_endpoint = state.endpoints.remove(&object_id);
        let stale_managed = removed_endpoint
            .as_ref()
            .and_then(|endpoint| endpoint.virtual_device_id)
            .or(removed_ingress)
            .and_then(|virtual_id| state.managed_virtuals.remove(&virtual_id));
        (removed_endpoint, stale_managed)
    };
    if let Some(managed) = stale_managed {
        destroy_managed_resource(managed.resource);
    }
    if let Some(endpoint) = removed_endpoint {
        send_event(
            events,
            BackendEvent::EndpointRemoved {
                endpoint_id: endpoint.id,
            },
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_command<'core>(
    command: BackendCommand,
    context: &pw::context::ContextRc,
    core: &'core pw::core::CoreRc,
    state: &Rc<RefCell<RegistryState>>,
    runtimes: &mut Runtimes<'core>,
    pending_connects: &mut Vec<PendingConnect>,
    hub: &std::sync::Arc<ControlHub>,
    events: &Sender<BackendEvent>,
) -> bool {
    match command {
        BackendCommand::Shutdown { command_id } => {
            send_event(
                events,
                BackendEvent::Status {
                    status: BackendStatus::Stopping,
                },
            );
            send_event(events, BackendEvent::ShutdownComplete { command_id });
            true
        }
        BackendCommand::CreateVirtual {
            command_id,
            virtual_device_id,
            endpoint,
        } => {
            let result = create_virtual_node(
                context,
                core,
                state,
                virtual_device_id,
                endpoint.endpoint_type,
                &endpoint.name,
            );
            match result {
                Ok(()) => send_event(events, BackendEvent::CommandCompleted { command_id }),
                Err(error) => send_event(events, BackendEvent::CommandFailed { command_id, error }),
            }
            false
        }
        BackendCommand::DeleteVirtual {
            command_id,
            virtual_device_id,
        } => {
            // Tear down routes through the device BEFORE destroying its node:
            // destroying a node with linked streams makes PipeWire emit a core
            // protocol error that would otherwise stop the backend.
            let endpoint_id = state
                .borrow()
                .endpoints
                .values()
                .find(|endpoint| endpoint.virtual_device_id == Some(virtual_device_id))
                .map(|endpoint| endpoint.id);
            if let Some(endpoint_id) = endpoint_id {
                let affected: Vec<RouteId> = runtimes
                    .routes
                    .values()
                    .filter(|route| route.source == endpoint_id || route.destination == endpoint_id)
                    .map(|route| route.id)
                    .collect();
                for route_id in affected {
                    if disconnect_route(runtimes, route_id).is_ok() {
                        send_event(events, BackendEvent::RouteRemoved { route_id });
                    }
                }
                // Orphaned capture/bus runtimes go away with their last route.
                remove_runtime_if_orphaned(runtimes, endpoint_id, true);
                remove_runtime_if_orphaned(runtimes, endpoint_id, false);
            }
            let result = delete_virtual_node(core, state, virtual_device_id);
            match result {
                Ok(()) => send_event(events, BackendEvent::CommandCompleted { command_id }),
                Err(error) => send_event(events, BackendEvent::CommandFailed { command_id, error }),
            }
            false
        }
        BackendCommand::Connect { command_id, route } => {
            // Never stack two links on the same pair: a duplicate Connect
            // (e.g. reconciler re-firing while the first was in flight)
            // replaces the existing one instead of orphaning engine state.
            let existing = runtimes
                .routes
                .values()
                .find(|existing| {
                    existing.source == route.source && existing.destination == route.destination
                })
                .map(|existing| existing.id);
            if let Some(existing_id) = existing {
                let _ = disconnect_route(runtimes, existing_id);
                send_event(
                    events,
                    BackendEvent::RouteRemoved {
                        route_id: existing_id,
                    },
                );
            }
            let route_for_retry = route.clone();
            let result = connect_route(core, state, runtimes, route, hub);
            match result {
                Ok(route) => {
                    send_event(events, BackendEvent::RouteAdded { route });
                    send_event(events, BackendEvent::CommandCompleted { command_id });
                }
                Err(error) if ingress_pending(&error) => {
                    // The loopback's hidden ingress node may not be registered
                    // yet; retry on a timer instead of failing the user. One
                    // pending entry per pair: a duplicate Connect replaces it.
                    pending_connects.retain(|pending| {
                        let existing = &pending.route;
                        !(existing.source == route_for_retry.source
                            && existing.destination == route_for_retry.destination)
                    });
                    pending_connects.push(PendingConnect {
                        command_id,
                        route: route_for_retry,
                        attempts: 0,
                        last_attempt: Instant::now(),
                    });
                }
                Err(error) => send_event(events, BackendEvent::CommandFailed { command_id, error }),
            }
            false
        }
        BackendCommand::Disconnect {
            command_id,
            route_id,
        } => {
            let result = disconnect_route(runtimes, route_id);
            match result {
                Ok(()) => {
                    send_event(events, BackendEvent::RouteRemoved { route_id });
                    send_event(events, BackendEvent::CommandCompleted { command_id });
                }
                Err(error) => send_event(events, BackendEvent::CommandFailed { command_id, error }),
            }
            false
        }
        BackendCommand::SetVolume {
            command_id,
            endpoint_id,
            gain,
        } => {
            let result = update_endpoint_control(state, hub, endpoint_id, |endpoint, controls| {
                endpoint.gain = gain;
                controls.set_gain(gain);
            });
            finish_control_command(result, command_id, events);
            false
        }
        BackendCommand::SetMute {
            command_id,
            endpoint_id,
            muted,
        } => {
            let result = update_endpoint_control(state, hub, endpoint_id, |endpoint, controls| {
                endpoint.muted = muted;
                controls.set_muted(muted);
            });
            finish_control_command(result, command_id, events);
            false
        }
        BackendCommand::SetBalance {
            command_id,
            endpoint_id,
            balance,
        } => {
            let result = update_endpoint_control(state, hub, endpoint_id, |endpoint, controls| {
                endpoint.balance = balance;
                controls.set_balance(balance);
            });
            finish_control_command(result, command_id, events);
            false
        }
        BackendCommand::SetStreamTuning {
            command_id,
            stream_rate,
            buffer_frames,
        } => {
            let rate_changed = hub.set_stream_rate(stream_rate);
            let buffer_changed = hub.set_buffer_frames(buffer_frames);
            if rate_changed || buffer_changed {
                // Immediate effect: rebuild active streams so the new
                // rate/latency applies without re-patching. Brief dropout on
                // active routes is the accepted trade-off.
                rebuild_all_streams(core, state, runtimes, hub, events);
            }
            send_event(events, BackendEvent::CommandCompleted { command_id });
            false
        }
        BackendCommand::SetDelay {
            command_id,
            endpoint_id,
            delay_ms,
        } => {
            hub.set_delay_ms(endpoint_id, delay_ms);
            send_event(events, BackendEvent::CommandCompleted { command_id });
            false
        }
        BackendCommand::SetEq {
            command_id,
            endpoint_id,
            low_db,
            mid_db,
            high_db,
        } => {
            hub.set_eq_db(endpoint_id, low_db, mid_db, high_db);
            send_event(events, BackendEvent::CommandCompleted { command_id });
            false
        }
        BackendCommand::SetChannelMode {
            command_id,
            endpoint_id,
            mode,
        } => {
            hub.set_channel_mode(endpoint_id, mode);
            send_event(events, BackendEvent::CommandCompleted { command_id });
            false
        }
    }
}

/// Rebuild streams that report Streaming but process no buffers: streams
/// can stall when devices suspend mid-connect. Bounded attempts per
/// endpoint; routes survive — their rings are relinked into fresh streams.
fn rebuild_stalled_streams<'core>(
    core: &'core pw::core::CoreRc,
    state: &Rc<RefCell<RegistryState>>,
    runtimes: &mut Runtimes<'core>,
    hub: &std::sync::Arc<ControlHub>,
    events: &Sender<BackendEvent>,
    budget: &mut HashMap<EndpointId, (u8, Instant)>,
) {
    const MAX_ATTEMPTS: u8 = 3;
    const COOLDOWN: Duration = Duration::from_secs(30);

    let endpoint_suspended = |endpoint_id: EndpointId| {
        let state = state.borrow();
        state
            .endpoints
            .values()
            .find(|endpoint| endpoint.id == endpoint_id)
            .is_some_and(|endpoint| matches!(endpoint.state, EndpointState::Suspended))
    };
    let mut stalled: Vec<(EndpointId, bool)> = Vec::new(); // (endpoint, is_capture)
    for (endpoint_id, runtime) in runtimes.captures.iter_mut() {
        if runtime.is_stalled() && !endpoint_suspended(*endpoint_id) {
            stalled.push((*endpoint_id, true));
        }
    }
    for (endpoint_id, runtime) in runtimes.buses.iter_mut() {
        if runtime.is_stalled() && !endpoint_suspended(*endpoint_id) {
            stalled.push((*endpoint_id, false));
        }
    }

    for (endpoint_id, is_capture) in stalled {
        let (attempts, last) = budget.entry(endpoint_id).or_insert((0, Instant::now()));
        if last.elapsed() >= COOLDOWN {
            *attempts = 0;
        }
        if *attempts >= MAX_ATTEMPTS {
            continue;
        }
        *attempts += 1;
        *last = Instant::now();
        log::warn!(
            "{} stream for endpoint {} stalled (streaming but silent); rebuilding",
            if is_capture { "capture" } else { "playback" },
            endpoint_id
        );
        rebuild_runtime(core, state, runtimes, hub, endpoint_id, is_capture, events);
    }
}

/// Recreate one endpoint's stream and relink every route touching it with
/// fresh rings. Route identity is preserved; the far side's engine swaps
/// the route's ring half through its inbox.
fn rebuild_runtime<'core>(
    core: &'core pw::core::CoreRc,
    state: &Rc<RefCell<RegistryState>>,
    runtimes: &mut Runtimes<'core>,
    hub: &std::sync::Arc<ControlHub>,
    endpoint_id: EndpointId,
    is_capture: bool,
    events: &Sender<BackendEvent>,
) {
    let affected: Vec<AudioRoute> = runtimes
        .routes
        .values()
        .filter(|route| {
            if is_capture {
                route.source == endpoint_id
            } else {
                route.destination == endpoint_id
            }
        })
        .cloned()
        .collect();
    // Detach the routes from both sides, drop the stream, recreate it, and
    // relink. Route events keep the UI matrix consistent.
    for route in &affected {
        let _ = unlink_route(runtimes, route.id);
        send_event(events, BackendEvent::RouteRemoved { route_id: route.id });
    }
    if is_capture {
        if let Some(runtime) = runtimes.captures.remove(&endpoint_id) {
            let _ = runtime.disconnect();
        }
    } else if let Some(runtime) = runtimes.buses.remove(&endpoint_id) {
        let _ = runtime.disconnect();
    }
    for route in affected {
        match link_route(core, state, runtimes, route.clone(), hub) {
            Ok(route) => send_event(events, BackendEvent::RouteAdded { route }),
            Err(error) => {
                log::error!(
                    "route {} failed to relink after stream rebuild: {}",
                    route.id,
                    error.technical_message
                );
                send_event(events, BackendEvent::Error { error });
            }
        }
    }
}

/// Tear down and recreate every stream against the current tuning, emitting
/// route events so the UI matrix stays consistent.
fn rebuild_all_streams<'core>(
    core: &'core pw::core::CoreRc,
    state: &Rc<RefCell<RegistryState>>,
    runtimes: &mut Runtimes<'core>,
    hub: &std::sync::Arc<ControlHub>,
    events: &Sender<BackendEvent>,
) {
    let active: Vec<AudioRoute> = runtimes.routes.values().cloned().collect();
    runtimes.disconnect_all();
    for route in active {
        match connect_route(core, state, runtimes, route.clone(), hub) {
            Ok(route) => send_event(events, BackendEvent::RouteAdded { route }),
            Err(error) => {
                log::error!(
                    "route {} failed to rebuild: {}",
                    route.id,
                    error.technical_message
                );
                send_event(events, BackendEvent::Error { error });
            }
        }
    }
}

struct PendingConnect {
    command_id: crate::domain::CommandId,
    route: crate::domain::AudioRoute,
    attempts: u8,
    last_attempt: Instant,
}

fn ingress_pending(error: &AudioError) -> bool {
    error.retryable && error.technical_message.contains("ingress")
}

fn retry_pending_connects<'core>(
    core: &'core pw::core::CoreRc,
    state: &Rc<RefCell<RegistryState>>,
    runtimes: &mut Runtimes<'core>,
    pending_connects: &mut Vec<PendingConnect>,
    hub: &std::sync::Arc<ControlHub>,
    events: &Sender<BackendEvent>,
) {
    let mut index = 0;
    while index < pending_connects.len() {
        let pending = &pending_connects[index];
        if pending.last_attempt.elapsed() < CONNECT_RETRY_INTERVAL {
            index += 1;
            continue;
        }
        let command_id = pending.command_id;
        let route = pending.route.clone();
        match connect_route(core, state, runtimes, route.clone(), hub) {
            Ok(route) => {
                send_event(events, BackendEvent::RouteAdded { route });
                send_event(events, BackendEvent::CommandCompleted { command_id });
                pending_connects.remove(index);
            }
            Err(error) if ingress_pending(&error) => {
                let pending = &mut pending_connects[index];
                pending.attempts += 1;
                pending.last_attempt = Instant::now();
                if pending.attempts >= CONNECT_RETRY_LIMIT {
                    let pending = pending_connects.remove(index);
                    send_event(
                        events,
                        BackendEvent::CommandFailed {
                            command_id: pending.command_id,
                            error,
                        },
                    );
                }
            }
            Err(error) => {
                let pending = pending_connects.remove(index);
                send_event(
                    events,
                    BackendEvent::CommandFailed {
                        command_id: pending.command_id,
                        error,
                    },
                );
            }
        }
    }
}

/// Validate a route request, then link it: ensure both endpoint streams
/// exist and deliver a fresh ring into their engines.
fn connect_route<'core>(
    core: &'core pw::core::CoreRc,
    state: &Rc<RefCell<RegistryState>>,
    runtimes: &mut Runtimes<'core>,
    route: AudioRoute,
    hub: &std::sync::Arc<ControlHub>,
) -> Result<AudioRoute, AudioError> {
    if runtimes.routes.contains_key(&route.id) {
        return Err(route_command_error(
            ErrorCode::DuplicateRoute,
            "This audio route is already active.",
            format!("route {} already has a PipeWire link", route.id),
        ));
    }
    if runtimes.routes.values().any(|existing| {
        existing.source == route.source && existing.destination == route.destination
    }) {
        return Err(route_command_error(
            ErrorCode::DuplicateRoute,
            "Those devices are already connected.",
            format!(
                "duplicate PipeWire route from {} to {}",
                route.source, route.destination
            ),
        ));
    }

    let mut route = route;
    route.state = RouteState::Connecting;
    let linked = link_route(core, state, runtimes, route.clone(), hub)?;
    runtimes.routes.insert(linked.id, linked.clone());
    Ok(linked)
}

/// The inner half of connect_route, also used when relinking routes into a
/// rebuilt stream. Assumes the route is not currently linked.
fn link_route<'core>(
    core: &'core pw::core::CoreRc,
    state: &Rc<RefCell<RegistryState>>,
    runtimes: &mut Runtimes<'core>,
    route: AudioRoute,
    hub: &std::sync::Arc<ControlHub>,
) -> Result<AudioRoute, AudioError> {
    let (source, destination) = resolve_route_endpoints(state, &route)?;
    if !source.endpoint_type.can_source() || !destination.endpoint_type.can_receive() {
        return Err(route_command_error(
            ErrorCode::InvalidRoute,
            "Those devices cannot be connected.",
            format!(
                "invalid PipeWire route direction from {:?} to {:?}",
                source.endpoint_type, destination.endpoint_type
            ),
        ));
    }

    // Ensure both endpoint streams exist. If the bus side fails after the
    // capture side was created, drop the capture again when it has no other
    // routes so a failed connect never leaves orphan streams.
    let capture_created = !runtimes.captures.contains_key(&source.id);
    if capture_created {
        let runtime = CaptureRuntime::new(core, &source, hub)?;
        runtimes.captures.insert(source.id, runtime);
    }
    let bus_created = !runtimes.buses.contains_key(&destination.id);
    if bus_created {
        match PlaybackRuntime::new(core, &destination, hub) {
            Ok(runtime) => {
                runtimes.buses.insert(destination.id, runtime);
            }
            Err(error) => {
                if capture_created {
                    if let Some(runtime) = runtimes.captures.remove(&source.id) {
                        let _ = runtime.disconnect();
                    }
                }
                return Err(error);
            }
        }
    }

    // Create the route's ring and deliver its halves to both engines. The
    // corrector targets one quantum of buffering (validated up to two).
    let bus_channels = runtimes
        .buses
        .get(&destination.id)
        .map(|runtime| runtime.channels())
        .unwrap_or(2);
    let link = RouteLink::new(route.id, bus_channels, hub.buffer_frames(), TARGET_QUANTA).and_then(
        |link| {
            let source_generation = runtimes
                .captures
                .get(&source.id)
                .map(|runtime| runtime.next_generation())
                .unwrap_or(1);
            let bus_generation = runtimes
                .buses
                .get(&destination.id)
                .map(|runtime| runtime.next_generation())
                .unwrap_or(1);
            let balance = hub.endpoint(source.id).balance_gains();
            link.into_halves(source_generation, bus_generation, balance)
        },
    );
    let (source_half, bus_half) = match link {
        Ok(halves) => halves,
        Err(error) => {
            let error = route_command_error(
                ErrorCode::InvalidRoute,
                "Orion could not start realtime processing for that connection.",
                format!("failed to create route link for {}: {error}", route.id),
            );
            remove_runtime_if_orphaned(runtimes, source.id, true);
            remove_runtime_if_orphaned(runtimes, destination.id, false);
            return Err(error);
        }
    };

    let feed_result = runtimes
        .captures
        .get_mut(&source.id)
        .expect("capture runtime ensured")
        .add_feed(route.id, bus_channels, source_half);
    if let Err(error) = feed_result {
        remove_runtime_if_orphaned(runtimes, source.id, true);
        remove_runtime_if_orphaned(runtimes, destination.id, false);
        return Err(error);
    }
    let draw_result = runtimes
        .buses
        .get_mut(&destination.id)
        .expect("bus runtime ensured")
        .add_draw(route.id, bus_half);
    if let Err(error) = draw_result {
        let _ = runtimes
            .captures
            .get_mut(&source.id)
            .expect("capture runtime ensured")
            .remove_feed(route.id);
        remove_runtime_if_orphaned(runtimes, source.id, true);
        remove_runtime_if_orphaned(runtimes, destination.id, false);
        return Err(error);
    }
    Ok(route)
}

/// Resolve both endpoints of a route against the registry, substituting the
/// virtual output's hidden ingress node as its stream target.
fn resolve_route_endpoints(
    state: &Rc<RefCell<RegistryState>>,
    route: &AudioRoute,
) -> Result<(AudioEndpoint, AudioEndpoint), AudioError> {
    let state = state.borrow();
    let source = state
        .endpoints
        .values()
        .find(|endpoint| endpoint.id == route.source)
        .cloned()
        .ok_or_else(|| {
            route_command_error(
                ErrorCode::EndpointNotFound,
                "The connection source is no longer available.",
                format!(
                    "source endpoint {} not found in PipeWire registry",
                    route.source
                ),
            )
        })?;
    let mut destination = state
        .endpoints
        .values()
        .find(|endpoint| endpoint.id == route.destination)
        .cloned()
        .ok_or_else(|| {
            route_command_error(
                ErrorCode::EndpointNotFound,
                "The connection destination is no longer available.",
                format!(
                    "destination endpoint {} not found in PipeWire registry",
                    route.destination
                ),
            )
        })?;
    if destination.endpoint_type == EndpointType::VirtualOutput {
        let virtual_device_id = destination.virtual_device_id.ok_or_else(|| {
            route_command_error(
                ErrorCode::InvalidRoute,
                "The virtual output is missing its identity.",
                format!("virtual output {} has no virtual device id", destination.id),
            )
        })?;
        destination.runtime_id = Some(
            state
                .virtual_ingresses
                .get(&virtual_device_id)
                .copied()
                .ok_or_else(|| {
                    route_command_error(
                        ErrorCode::EndpointNotFound,
                        "The virtual output is still starting up — try again in a moment.",
                        format!("virtual output ingress {virtual_device_id} is unavailable"),
                    )
                })?,
        );
    }
    Ok((source, destination))
}

/// Detach a route from both engines; endpoint streams are reference-counted
/// and torn down when their last route leaves.
fn unlink_route(runtimes: &mut Runtimes<'_>, route_id: RouteId) -> Result<(), AudioError> {
    let Some(route) = runtimes.routes.get(&route_id).cloned() else {
        return Ok(());
    };
    if let Some(runtime) = runtimes.captures.get_mut(&route.source) {
        runtime.remove_feed(route_id)?;
    }
    if let Some(runtime) = runtimes.buses.get_mut(&route.destination) {
        runtime.remove_draw(route_id)?;
    }
    remove_runtime_if_orphaned(runtimes, route.source, true);
    remove_runtime_if_orphaned(runtimes, route.destination, false);
    Ok(())
}

/// Idempotent: an unknown route is already gone, which is the desired end
/// state — report removal instead of failing (double disconnects happen when
/// the UI reconciles faster than route events round-trip).
fn disconnect_route(runtimes: &mut Runtimes<'_>, route_id: RouteId) -> Result<(), AudioError> {
    unlink_route(runtimes, route_id)?;
    runtimes.routes.remove(&route_id);
    Ok(())
}

fn remove_runtime_if_orphaned(runtimes: &mut Runtimes<'_>, endpoint_id: EndpointId, capture: bool) {
    if capture {
        let orphaned = runtimes
            .captures
            .get(&endpoint_id)
            .is_some_and(|runtime| !runtime.has_routes());
        if orphaned {
            if let Some(runtime) = runtimes.captures.remove(&endpoint_id) {
                let _ = runtime.disconnect();
            }
        }
    } else {
        let orphaned = runtimes
            .buses
            .get(&endpoint_id)
            .is_some_and(|runtime| !runtime.has_routes());
        if orphaned {
            if let Some(runtime) = runtimes.buses.remove(&endpoint_id) {
                let _ = runtime.disconnect();
            }
        }
    }
}

/// Returns Some(endpoint) only when the control update actually changed
/// something — an idempotent command must not echo an EndpointUpdated event
/// or UI-side re-push loops would never settle. The shared engine controls
/// for the endpoint are updated in the hub, where every callback reads them.
fn update_endpoint_control(
    state: &Rc<RefCell<RegistryState>>,
    hub: &std::sync::Arc<ControlHub>,
    endpoint_id: EndpointId,
    update: impl FnOnce(&mut AudioEndpoint, &crate::realtime::EndpointControls),
) -> Result<Option<AudioEndpoint>, AudioError> {
    let mut state = state.borrow_mut();
    let endpoint = state
        .endpoints
        .values_mut()
        .find(|endpoint| endpoint.id == endpoint_id)
        .ok_or_else(|| {
            route_command_error(
                ErrorCode::EndpointNotFound,
                "That device is no longer available.",
                format!("endpoint {endpoint_id} not found in PipeWire registry"),
            )
        })?;
    let before = (endpoint.gain, endpoint.muted, endpoint.balance);
    let controls = hub.endpoint(endpoint_id);
    update(endpoint, &controls);
    let changed = before != (endpoint.gain, endpoint.muted, endpoint.balance);
    let endpoint = endpoint.clone();
    drop(state);
    Ok(changed.then_some(endpoint))
}

fn finish_control_command(
    result: Result<Option<AudioEndpoint>, AudioError>,
    command_id: crate::domain::CommandId,
    events: &Sender<BackendEvent>,
) {
    match result {
        Ok(Some(endpoint)) => {
            send_event(events, BackendEvent::EndpointUpdated { endpoint });
            send_event(events, BackendEvent::CommandCompleted { command_id });
        }
        Ok(None) => send_event(events, BackendEvent::CommandCompleted { command_id }),
        Err(error) => send_event(events, BackendEvent::CommandFailed { command_id, error }),
    }
}

fn prune_unavailable_routes(
    runtimes: &mut Runtimes<'_>,
    state: &Rc<RefCell<RegistryState>>,
    events: &Sender<BackendEvent>,
) {
    let state = state.borrow();
    let unavailable = runtimes
        .routes
        .values()
        .filter_map(|route| {
            let source_available = state
                .endpoints
                .values()
                .any(|endpoint| endpoint.id == route.source);
            let destination_available = state
                .endpoints
                .values()
                .any(|endpoint| endpoint.id == route.destination);
            (!source_available || !destination_available).then_some(route.id)
        })
        .collect::<Vec<_>>();
    drop(state);

    for route_id in unavailable {
        if disconnect_route(runtimes, route_id).is_ok() {
            send_event(events, BackendEvent::RouteRemoved { route_id });
        }
    }
}

fn refresh_route_states(runtimes: &mut Runtimes<'_>, events: &Sender<BackendEvent>) {
    for runtime in runtimes.captures.values_mut() {
        runtime.ensure_active();
    }
    for runtime in runtimes.buses.values_mut() {
        runtime.ensure_active();
    }
    let route_ids: Vec<RouteId> = runtimes.routes.keys().copied().collect();
    for route_id in route_ids {
        let (state, changed_route) = {
            let Some(route) = runtimes.routes.get(&route_id) else {
                continue;
            };
            let state = runtimes.route_state(route);
            if state == route.state {
                continue;
            }
            let route = runtimes.routes.get_mut(&route_id).expect("route present");
            route.state = state;
            (state, route.clone())
        };
        let _ = state;
        send_event(
            events,
            BackendEvent::RouteUpdated {
                route: changed_route,
            },
        );
    }
}

fn route_command_error(
    code: ErrorCode,
    user: impl Into<String>,
    technical: impl Into<String>,
) -> AudioError {
    AudioError::new(code, ErrorSeverity::Error, true, user, technical)
}

fn endpoint_from_global(
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
) -> Option<AudioEndpoint> {
    if global.type_ != ObjectType::Node {
        return None;
    }
    let props = global.props?;
    let media_class = props.get(*pw::keys::MEDIA_CLASS)?;
    let node_name = props.get(*pw::keys::NODE_NAME)?;
    if is_orion_processing_stream(props) {
        return None;
    }
    let is_virtual = media_class.contains("Virtual")
        || property_is_true(props.get(*pw::keys::NODE_VIRTUAL))
        || node_name.starts_with("orion.");
    let endpoint_type = endpoint_type(media_class, is_virtual)?;
    let identity = endpoint_identity(props, media_class, node_name);
    let endpoint_id = stable_endpoint_id(&identity, endpoint_type);
    let channel_count = inferred_channel_count(props);
    let channels = (0..channel_count)
        .map(|index| stable_channel_id(endpoint_id, index))
        .collect();

    let virtual_device_id = props
        .get(ORION_VIRTUAL_ID)
        .and_then(|value| Uuid::parse_str(value).ok())
        .map(VirtualDeviceId::from_uuid)
        .or_else(|| virtual_id_from_name(node_name, "orion.virtual-input."))
        .or_else(|| virtual_id_from_name(node_name, "orion.virtual-output."));

    Some(AudioEndpoint {
        id: endpoint_id,
        runtime_id: Some(global.id),
        device_id: stable_device_id(&identity),
        virtual_device_id,
        identity,
        name: props
            .get(*pw::keys::NODE_DESCRIPTION)
            .or_else(|| props.get(*pw::keys::NODE_NICK))
            .or_else(|| props.get(*pw::keys::MEDIA_NAME))
            .unwrap_or(node_name)
            .to_owned(),
        description: props
            .get(*pw::keys::NODE_DESCRIPTION)
            .or_else(|| props.get(*pw::keys::MEDIA_NAME))
            .unwrap_or(node_name)
            .to_owned(),
        endpoint_type,
        state: EndpointState::Available,
        channel_count,
        sample_rate: parse_property(props.get(*pw::keys::AUDIO_RATE)),
        is_default: false,
        channels,
        gain: GainDb::new(0.0).ok()?,
        muted: false,
        balance: NormalizedBalance::new(0.0).ok()?,
    })
}

fn endpoint_identity(
    props: &pw::spa::utils::dict::DictRef,
    media_class: &str,
    node_name: &str,
) -> EndpointIdentity {
    let mut identity = EndpointIdentity::new("pipewire");
    identity.serial = props.get(*pw::keys::DEVICE_SERIAL).map(str::to_owned);
    identity.bus = props.get(*pw::keys::DEVICE_BUS).map(str::to_owned);
    identity.vendor = props.get(*pw::keys::DEVICE_VENDOR_ID).map(str::to_owned);
    identity.product = props.get(*pw::keys::DEVICE_PRODUCT_ID).map(str::to_owned);
    identity.device_name = props
        .get(*pw::keys::DEVICE_NAME)
        .or_else(|| props.get(*pw::keys::DEVICE_DESCRIPTION))
        .map(str::to_owned);
    identity.node_name = Some(node_name.to_owned());
    identity.profile = props.get("device.profile.name").map(str::to_owned);
    identity.media_class = Some(media_class.to_owned());
    identity.alsa_path = props.get("api.alsa.path").map(str::to_owned);
    identity
}

fn endpoint_type(media_class: &str, is_virtual: bool) -> Option<EndpointType> {
    if media_class.starts_with("Audio/Source") {
        Some(if is_virtual {
            EndpointType::VirtualOutput
        } else {
            EndpointType::PhysicalInput
        })
    } else if media_class.starts_with("Audio/Sink") {
        Some(if is_virtual {
            EndpointType::VirtualInput
        } else {
            EndpointType::PhysicalOutput
        })
    } else if media_class.starts_with("Stream/Output/Audio") {
        Some(EndpointType::ApplicationOutput)
    } else if media_class.starts_with("Stream/Input/Audio") {
        Some(EndpointType::ApplicationInput)
    } else {
        None
    }
}

fn stable_endpoint_id(identity: &EndpointIdentity, endpoint_type: EndpointType) -> EndpointId {
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

fn stable_device_id(identity: &EndpointIdentity) -> Option<DeviceId> {
    let key = identity
        .serial
        .as_deref()
        .or(identity.device_name.as_deref())?;
    Some(DeviceId::from_uuid(Uuid::new_v5(
        &ORION_ID_NAMESPACE,
        format!("device|{key}").as_bytes(),
    )))
}

fn stable_channel_id(endpoint_id: EndpointId, index: u32) -> ChannelId {
    ChannelId::from_uuid(Uuid::new_v5(
        &ORION_ID_NAMESPACE,
        format!("channel|{endpoint_id}|{index}").as_bytes(),
    ))
}

fn virtual_id_from_name(name: &str, prefix: &str) -> Option<VirtualDeviceId> {
    name.strip_prefix(prefix)
        .and_then(|value| Uuid::parse_str(value).ok())
        .map(VirtualDeviceId::from_uuid)
}

fn endpoint_state(state: pw::node::NodeState<'_>) -> EndpointState {
    match state {
        pw::node::NodeState::Creating => EndpointState::Available,
        pw::node::NodeState::Suspended => EndpointState::Suspended,
        pw::node::NodeState::Idle => EndpointState::Idle,
        pw::node::NodeState::Running => EndpointState::Running,
        pw::node::NodeState::Error(_) => EndpointState::Error,
    }
}

fn update_endpoint_properties(endpoint: &mut AudioEndpoint, props: &pw::spa::utils::dict::DictRef) {
    if let Some(description) = props.get(*pw::keys::NODE_DESCRIPTION) {
        endpoint.description = description.to_owned();
        endpoint.name = description.to_owned();
    }
    if let Some(channels) = parse_property(props.get(*pw::keys::AUDIO_CHANNELS)) {
        endpoint.channel_count = channels;
        endpoint.channels = (0..channels)
            .map(|index| stable_channel_id(endpoint.id, index))
            .collect();
    }
    if let Some(rate) = parse_property(props.get(*pw::keys::AUDIO_RATE)) {
        endpoint.sample_rate = Some(rate);
    }
}

fn refresh_defaults(state: &mut RegistryState, events: &Sender<BackendEvent>) {
    let source = state.defaults.source.clone();
    let sink = state.defaults.sink.clone();
    for endpoint in state.endpoints.values_mut() {
        let was_default = endpoint.is_default;
        endpoint.is_default = match endpoint.endpoint_type {
            EndpointType::PhysicalInput | EndpointType::VirtualOutput => {
                source.as_deref() == endpoint.identity.node_name.as_deref()
            }
            EndpointType::PhysicalOutput | EndpointType::VirtualInput => {
                sink.as_deref() == endpoint.identity.node_name.as_deref()
            }
            _ => false,
        };
        if endpoint.is_default != was_default {
            send_event(
                events,
                BackendEvent::EndpointUpdated {
                    endpoint: endpoint.clone(),
                },
            );
        }
    }
}

fn apply_default(endpoint: &mut AudioEndpoint, defaults: &DefaultEndpoints) {
    let name = endpoint.identity.node_name.as_deref();
    endpoint.is_default = match endpoint.endpoint_type {
        EndpointType::PhysicalInput | EndpointType::VirtualOutput => {
            defaults.source.as_deref() == name
        }
        EndpointType::PhysicalOutput | EndpointType::VirtualInput => {
            defaults.sink.as_deref() == name
        }
        _ => false,
    };
}

fn default_node_name(value: Option<&str>) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(value?)
        .ok()?
        .get("name")?
        .as_str()
        .map(str::to_owned)
}

fn parse_property<T: std::str::FromStr>(value: Option<&str>) -> Option<T> {
    value.and_then(|value| value.parse().ok())
}

fn property_is_true(value: Option<&str>) -> bool {
    matches!(value, Some("true" | "1" | "yes"))
}

fn is_orion_processing_stream(props: &pw::spa::utils::dict::DictRef) -> bool {
    props
        .get(*pw::keys::MEDIA_CLASS)
        .is_some_and(|media_class| media_class.starts_with("Stream/"))
        && props.get("application.id") == Some(ORION_APPLICATION_ID)
}

fn inferred_channel_count(props: &pw::spa::utils::dict::DictRef) -> u32 {
    parse_property(props.get(*pw::keys::AUDIO_CHANNELS))
        .or_else(|| {
            props.get("audio.position").map(|position| {
                position
                    .trim_matches(|character| character == '[' || character == ']')
                    .split(',')
                    .filter(|channel| !channel.trim().is_empty())
                    .count() as u32
            })
        })
        .filter(|channels| *channels != 0)
        .unwrap_or(2)
}

fn owned_properties(props: &pw::spa::utils::dict::DictRef) -> HashMap<String, String> {
    props
        .iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn send_event(events: &Sender<BackendEvent>, event: BackendEvent) {
    let _ = events.send(event);
}

fn create_default_virtuals(
    context: &pw::context::ContextRc,
    core: &pw::core::CoreRc,
    state: &Rc<RefCell<RegistryState>>,
    events: &Sender<BackendEvent>,
) {
    for (virtual_device_id, endpoint_type, name) in default_virtual_specs() {
        if let Err(error) = create_virtual_node(
            context,
            core,
            state,
            virtual_device_id,
            endpoint_type,
            &name,
        ) {
            send_event(events, BackendEvent::Error { error });
        }
    }
}

fn reconcile_default_virtuals(
    context: &pw::context::ContextRc,
    core: &pw::core::CoreRc,
    state: &Rc<RefCell<RegistryState>>,
    events: &Sender<BackendEvent>,
) {
    for (virtual_device_id, endpoint_type, name) in default_virtual_specs() {
        let live = {
            let state = state.borrow();
            virtual_is_live(&state, virtual_device_id, endpoint_type)
        };
        if live {
            continue;
        }
        if let Err(error) = create_virtual_node(
            context,
            core,
            state,
            virtual_device_id,
            endpoint_type,
            &name,
        ) {
            send_event(
                events,
                BackendEvent::Error {
                    error: AudioError::new(
                        error.code,
                        ErrorSeverity::Warning,
                        true,
                        error.user_message,
                        error.technical_message,
                    ),
                },
            );
        }
    }
}

fn default_virtual_specs() -> Vec<(VirtualDeviceId, EndpointType, String)> {
    let mut specs = Vec::with_capacity(4);
    for (index, id) in DEFAULT_VIRTUAL_INPUT_IDS.into_iter().enumerate() {
        specs.push((
            VirtualDeviceId::from_uuid(Uuid::from_u128(id)),
            EndpointType::VirtualInput,
            format!("Orion Virtual Input {}", index + 1),
        ));
    }
    for (index, id) in DEFAULT_VIRTUAL_OUTPUT_IDS.into_iter().enumerate() {
        specs.push((
            VirtualDeviceId::from_uuid(Uuid::from_u128(id)),
            EndpointType::VirtualOutput,
            format!("Orion Virtual Output {}", index + 1),
        ));
    }
    specs
}

fn virtual_is_live(
    state: &RegistryState,
    virtual_device_id: VirtualDeviceId,
    endpoint_type: EndpointType,
) -> bool {
    let has_endpoint = state.endpoints.values().any(|endpoint| {
        endpoint.virtual_device_id == Some(virtual_device_id)
            && endpoint.endpoint_type == endpoint_type
    });
    match endpoint_type {
        EndpointType::VirtualOutput => {
            has_endpoint && state.virtual_ingresses.contains_key(&virtual_device_id)
        }
        EndpointType::VirtualInput => has_endpoint,
        _ => false,
    }
}

fn create_virtual_node(
    context: &pw::context::ContextRc,
    core: &pw::core::CoreRc,
    state: &Rc<RefCell<RegistryState>>,
    virtual_device_id: VirtualDeviceId,
    endpoint_type: EndpointType,
    name: &str,
) -> Result<(), AudioError> {
    if !matches!(
        endpoint_type,
        EndpointType::VirtualInput | EndpointType::VirtualOutput
    ) {
        return Err(virtual_error(
            "Only virtual input and output devices can be created.",
            format!("invalid virtual endpoint type {endpoint_type:?}"),
        ));
    }
    let stale = {
        let state = state.borrow();
        if virtual_is_live(&state, virtual_device_id, endpoint_type) {
            return Ok(());
        }
        state.managed_virtuals.contains_key(&virtual_device_id)
    };
    if stale {
        if let Some(managed) = state
            .borrow_mut()
            .managed_virtuals
            .remove(&virtual_device_id)
        {
            destroy_managed_resource(managed.resource);
        }
        state
            .borrow_mut()
            .virtual_ingresses
            .remove(&virtual_device_id);
    }

    let resource = match endpoint_type {
        EndpointType::VirtualInput => {
            let mut props = pw::properties::PropertiesBox::new();
            props.insert("factory.name", "support.null-audio-sink");
            props.insert(
                "node.name",
                format!("orion.virtual-input.{virtual_device_id}"),
            );
            // Avoid node.virtual=true and idle suspend so KDE/Plasma lists these sinks.
            props.insert("node.description", name);
            props.insert("node.nick", name);
            props.insert("node.virtual", "false");
            props.insert("node.autoconnect", "false");
            props.insert("object.linger", "false");
            props.insert("session.suspend-timeout-seconds", "0");
            props.insert("application.id", ORION_APPLICATION_ID);
            props.insert("application.name", "Orion");
            props.insert(ORION_VIRTUAL_ID, virtual_device_id.to_string());
            props.insert(ORION_VIRTUAL_ROLE, "input");
            props.insert("media.class", "Audio/Sink");
            props.insert("media.role", "Music");
            props.insert("media.icon-name", "audio-card");
            props.insert("device.icon-name", "audio-card");
            props.insert("device.class", "sound");
            props.insert("priority.session", "1000");
            props.insert("audio.channels", "2");
            props.insert("audio.position", "FL,FR");
            props.insert("monitor.channel-volumes", "true");
            let node = core
                .create_object::<pw::node::Node>("adapter", &props)
                .map_err(|error| {
                    virtual_error(
                        "Orion could not create the virtual input.",
                        format!("failed to create input adapter {virtual_device_id}: {error}"),
                    )
                })?;
            ManagedVirtualResource::Node(node)
        }
        EndpointType::VirtualOutput => {
            let module = create_virtual_output_loopback(context, virtual_device_id, name)?;
            ManagedVirtualResource::Loopback(module)
        }
        _ => unreachable!(),
    };
    state.borrow_mut().managed_virtuals.insert(
        virtual_device_id,
        ManagedVirtualNode {
            resource,
            endpoint_type,
        },
    );
    Ok(())
}

fn delete_virtual_node(
    _core: &pw::core::CoreRc,
    state: &Rc<RefCell<RegistryState>>,
    virtual_device_id: VirtualDeviceId,
) -> Result<(), AudioError> {
    let managed = {
        let mut state = state.borrow_mut();
        let endpoint_type = state
            .managed_virtuals
            .get(&virtual_device_id)
            .map(|managed| managed.endpoint_type)
            .ok_or_else(|| {
                virtual_error(
                    "The virtual device no longer exists.",
                    format!("managed virtual {virtual_device_id} not found"),
                )
            })?;
        let same_direction = state
            .managed_virtuals
            .values()
            .filter(|managed| managed.endpoint_type == endpoint_type)
            .count();
        if same_direction <= 1 {
            return Err(virtual_error(
                "Orion needs at least one virtual device of each type.",
                format!("refused to delete last required {endpoint_type:?}"),
            ));
        }
        state.virtual_ingresses.remove(&virtual_device_id);
        state
            .managed_virtuals
            .remove(&virtual_device_id)
            .ok_or_else(|| {
                virtual_error(
                    "The virtual device no longer exists.",
                    format!("managed virtual {virtual_device_id} disappeared"),
                )
            })?
    };
    match managed.resource {
        ManagedVirtualResource::Node(node) => {
            // One destroy, no more: dropping the proxy sends the destroy
            // request; destroy_object would send a second one to the
            // already-gone object and PipeWire would answer with a fatal
            // core protocol error.
            drop(node);
            Ok(())
        }
        ManagedVirtualResource::Loopback(module) => {
            unsafe { pw_impl_module_destroy(module) };
            Ok(())
        }
    }
}

fn destroy_all_virtuals(_core: &pw::core::CoreRc, state: &Rc<RefCell<RegistryState>>) {
    let managed = state
        .borrow_mut()
        .managed_virtuals
        .drain()
        .map(|(_, managed)| managed)
        .collect::<Vec<_>>();
    state.borrow_mut().virtual_ingresses.clear();
    for managed in managed {
        match managed.resource {
            ManagedVirtualResource::Node(node) => {
                // Same as delete_virtual_node: drop alone destroys the object.
                drop(node);
            }
            ManagedVirtualResource::Loopback(module) => {
                destroy_managed_resource(ManagedVirtualResource::Loopback(module));
            }
        }
    }
}

fn destroy_managed_resource(resource: ManagedVirtualResource) {
    match resource {
        ManagedVirtualResource::Node(node) => {
            drop(node);
        }
        ManagedVirtualResource::Loopback(module) => unsafe {
            if !module.is_null() {
                pw_impl_module_destroy(module);
            }
        },
    }
}

fn create_virtual_output_loopback(
    context: &pw::context::ContextRc,
    virtual_device_id: VirtualDeviceId,
    name: &str,
) -> Result<*mut c_void, AudioError> {
    let ingress_name = format!("orion.virtual-output-ingress.{virtual_device_id}");
    let source_name = format!("orion.virtual-output.{virtual_device_id}");
    let args = format!(
        r#"{{
            node.description = {description}
            audio.position = [ FL FR ]
            capture.props = {{
                node.name = {ingress_name}
                node.description = {ingress_description}
                media.class = Stream/Input/Audio/Internal
                node.virtual = true
                node.hidden = true
                node.autoconnect = false
                node.passive = true
                object.linger = false
                application.id = {application_id}
                {virtual_id_key} = {virtual_id}
                {virtual_role_key} = output-ingress
                audio.channels = 2
                audio.position = [ FL FR ]
            }}
            playback.props = {{
                node.name = {source_name}
                node.description = {description}
                node.nick = {description}
                media.class = Audio/Source
                media.role = Production
                media.icon-name = audio-input-microphone
                device.icon-name = audio-input-microphone
                device.class = sound
                priority.session = 2000
                node.virtual = false
                node.autoconnect = false
                object.linger = false
                session.suspend-timeout-seconds = 0
                application.id = {application_id}
                application.name = Orion
                {virtual_id_key} = {virtual_id}
                {virtual_role_key} = output
                audio.channels = 2
                audio.position = [ FL FR ]
            }}
        }}"#,
        description = spa_string(name),
        ingress_name = spa_string(&ingress_name),
        ingress_description = spa_string(&format!("{name} ingress")),
        source_name = spa_string(&source_name),
        application_id = spa_string(ORION_APPLICATION_ID),
        virtual_id_key = ORION_VIRTUAL_ID,
        virtual_id = spa_string(&virtual_device_id.to_string()),
        virtual_role_key = ORION_VIRTUAL_ROLE,
    );
    let module_name = CString::new("libpipewire-module-loopback").map_err(|error| {
        virtual_error(
            "Orion could not configure the virtual audio output.",
            format!("invalid loopback module name: {error}"),
        )
    })?;
    let args = CString::new(args).map_err(|error| {
        virtual_error(
            "Orion could not configure the virtual audio output.",
            format!("invalid loopback properties for {virtual_device_id}: {error}"),
        )
    })?;
    let module = unsafe {
        pw_context_load_module(
            context.as_raw_ptr(),
            module_name.as_ptr(),
            args.as_ptr(),
            ptr::null_mut(),
        )
    };
    if module.is_null() {
        Err(virtual_error(
            "Orion could not create the virtual audio output.",
            format!("failed to load loopback module for {virtual_device_id}"),
        ))
    } else {
        Ok(module)
    }
}

fn spa_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn virtual_error(user: impl Into<String>, technical: impl Into<String>) -> AudioError {
    AudioError::new(
        ErrorCode::UnsupportedOperation,
        ErrorSeverity::Error,
        true,
        user,
        technical,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for the mute freeze: a control command that changes nothing
    /// must not emit an endpoint update, or UI re-push loops never settle.
    #[test]
    fn control_update_without_change_returns_none() {
        let state = Rc::new(RefCell::new(RegistryState::default()));
        let endpoint = AudioEndpoint {
            id: EndpointId::new(),
            runtime_id: Some(7),
            device_id: None,
            virtual_device_id: None,
            identity: EndpointIdentity::new("pipewire"),
            description: "test".into(),
            name: "test".into(),
            endpoint_type: EndpointType::VirtualOutput,
            state: EndpointState::Running,
            channel_count: 2,
            sample_rate: None,
            is_default: false,
            channels: vec![ChannelId::new(), ChannelId::new()],
            gain: GainDb::default(),
            muted: false,
            balance: NormalizedBalance::default(),
        };
        let endpoint_id = endpoint.id;
        state.borrow_mut().endpoints.insert(7, endpoint);
        let hub = std::sync::Arc::new(ControlHub::default());

        // Real change: emits.
        let result = update_endpoint_control(&state, &hub, endpoint_id, |endpoint, controls| {
            endpoint.muted = true;
            controls.set_muted(true);
        })
        .expect("endpoint present");
        assert!(result.is_some(), "a real mute change emits an update");
        assert!(
            hub.endpoint(endpoint_id).is_muted(),
            "the hub publishes the control to engine callbacks"
        );

        // Idempotent repeat: silent.
        let result = update_endpoint_control(&state, &hub, endpoint_id, |endpoint, controls| {
            endpoint.muted = true;
            controls.set_muted(true);
        })
        .expect("endpoint present");
        assert!(result.is_none(), "no-change mute must not echo an update");

        // And a change back emits again.
        let result = update_endpoint_control(&state, &hub, endpoint_id, |endpoint, controls| {
            endpoint.muted = false;
            controls.set_muted(false);
        })
        .expect("endpoint present");
        assert!(result.is_some());
        assert!(!hub.endpoint(endpoint_id).is_muted());
    }

    #[test]
    fn classifies_routeable_node_directions() {
        assert_eq!(
            endpoint_type("Audio/Source", false),
            Some(EndpointType::PhysicalInput)
        );
        assert_eq!(
            endpoint_type("Audio/Sink", false),
            Some(EndpointType::PhysicalOutput)
        );
        assert_eq!(
            endpoint_type("Audio/Sink", true),
            Some(EndpointType::VirtualInput)
        );
        assert_eq!(
            endpoint_type("Audio/Source/Virtual", true),
            Some(EndpointType::VirtualOutput)
        );
        assert_eq!(
            endpoint_type("Stream/Output/Audio", false),
            Some(EndpointType::ApplicationOutput)
        );
        assert_eq!(
            endpoint_type("Stream/Input/Audio", false),
            Some(EndpointType::ApplicationInput)
        );
    }

    #[test]
    fn stable_endpoint_ids_are_repeatable_and_directional() {
        let mut identity = EndpointIdentity::new("pipewire");
        identity.node_name = Some("alsa_input.usb-mic".into());
        let first = stable_endpoint_id(&identity, EndpointType::PhysicalInput);
        let second = stable_endpoint_id(&identity, EndpointType::PhysicalInput);
        let output = stable_endpoint_id(&identity, EndpointType::PhysicalOutput);

        assert_eq!(first, second);
        assert_ne!(first, output);
    }

    #[test]
    fn parses_and_clears_default_names() {
        assert_eq!(
            default_node_name(Some(r#"{ "name": "alsa_output.usb-dac" }"#)).as_deref(),
            Some("alsa_output.usb-dac")
        );
        assert_eq!(default_node_name(Some("invalid")), None);
        assert_eq!(default_node_name(None), None);
    }

    #[test]
    fn infers_channels_from_position_before_node_info_arrives() {
        let props = pw::properties::properties! {
            "audio.position" => "[ FL, FR ]",
        };
        assert_eq!(inferred_channel_count(props.dict()), 2);
    }

    #[test]
    fn excludes_orion_processing_streams_from_application_candidates() {
        let orion = pw::properties::properties! {
            *pw::keys::MEDIA_CLASS => "Stream/Output/Audio",
            "application.id" => ORION_APPLICATION_ID,
        };
        let external = pw::properties::properties! {
            *pw::keys::MEDIA_CLASS => "Stream/Output/Audio",
            "application.id" => "org.example.player",
        };

        assert!(is_orion_processing_stream(orion.dict()));
        assert!(!is_orion_processing_stream(external.dict()));
    }

    #[test]
    fn virtual_liveness_requires_endpoint_and_output_ingress() {
        let mut state = RegistryState::default();
        let virtual_id = VirtualDeviceId::from_uuid(Uuid::from_u128(DEFAULT_VIRTUAL_OUTPUT_IDS[0]));
        assert!(!virtual_is_live(
            &state,
            virtual_id,
            EndpointType::VirtualOutput
        ));

        let mut endpoint = crate::domain::AudioEndpoint {
            id: EndpointId::new(),
            runtime_id: Some(7),
            device_id: None,
            virtual_device_id: Some(virtual_id),
            identity: crate::domain::EndpointIdentity::new("pipewire"),
            name: "Orion Virtual Output 1".into(),
            description: "Orion Virtual Output 1".into(),
            endpoint_type: EndpointType::VirtualOutput,
            state: crate::domain::EndpointState::Available,
            channel_count: 2,
            sample_rate: Some(48_000),
            is_default: false,
            channels: Vec::new(),
            gain: crate::domain::GainDb::default(),
            muted: false,
            balance: crate::domain::NormalizedBalance::default(),
        };
        state.endpoints.insert(7, endpoint.clone());
        assert!(!virtual_is_live(
            &state,
            virtual_id,
            EndpointType::VirtualOutput
        ));
        state.virtual_ingresses.insert(virtual_id, 8);
        assert!(virtual_is_live(
            &state,
            virtual_id,
            EndpointType::VirtualOutput
        ));

        endpoint.endpoint_type = EndpointType::VirtualInput;
        endpoint.virtual_device_id = Some(VirtualDeviceId::from_uuid(Uuid::from_u128(
            DEFAULT_VIRTUAL_INPUT_IDS[0],
        )));
        let input_id = endpoint.virtual_device_id.unwrap();
        state.endpoints.insert(9, endpoint);
        assert!(virtual_is_live(
            &state,
            input_id,
            EndpointType::VirtualInput
        ));
    }
}

//! cpal-based backend for Windows (WASAPI) and macOS (CoreAudio).
//!
//! cpal drives each device's stream from its own high-priority thread —
//! exactly the independent-clock topology the platform-neutral engine in
//! `src/realtime` is built for: capture callbacks run `SourceEngine`s,
//! output callbacks run `BusEngine`s, and routes link them through rings
//! with drift correction. This adapter owns enumeration (polled; cpal has
//! no cross-platform hotplug events), stream config negotiation, and the
//! command/meter loops. Virtual devices and per-app streams do not exist
//! on these platforms without drivers/plugins, so the backend reports
//! `virtual_devices: false` and the UI hides those affordances.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender};

use super::{AudioBackend, BackendCommand, BackendEvent, BackendStatus};
use crate::domain::{
    stable_channel_id, stable_device_id, stable_endpoint_id, AudioEndpoint, AudioError, AudioRoute,
    BackendCapabilities, EndpointId, EndpointIdentity, EndpointState, EndpointType, ErrorCode,
    ErrorSeverity, GainDb, MeterFrame, MeterLevel, NormalizedBalance, RouteId, RouteState,
};
use crate::realtime::{
    BusEngine, BusPlan, BusPublisher, ControlHub, PlanSlot, RouteLink, RouteMeter, SourceEngine,
    SourcePlan, SourcePublisher, TARGET_QUANTA,
};

const METER_INTERVAL: Duration = Duration::from_millis(33);
const HOTPLUG_INTERVAL: Duration = Duration::from_secs(1);
const COMMAND_POLL: Duration = Duration::from_millis(10);

/// cpal backend handle. Constructed empty; everything happens in `run`.
pub struct CpalBackend;

/// What drives a capture runtime's engine: a cpal device stream, or (on
/// Windows) a WASAPI process-loopback capture for application sources.
/// Fields are never read — they are the RAII resource: dropping the runtime
/// stops the stream/capture.
#[allow(dead_code)]
enum CaptureDriver {
    Device(cpal::Stream),
    #[cfg(target_os = "windows")]
    Loopback(super::windows_loopback::LoopbackCapture),
    #[cfg(target_os = "macos")]
    Tap(super::macos_tap::TapCapture),
}

/// Capture stream runtime for one source endpoint: the engine lives inside
/// the driver's data callback; plan management lives in the publisher.
struct CaptureRuntime {
    endpoint_id: EndpointId,
    meter: Arc<RouteMeter>,
    publisher: SourcePublisher,
    activity: Arc<AtomicU64>,
    _driver: CaptureDriver,
}

/// Playback (bus) runtime for one destination endpoint.
struct BusRuntime {
    endpoint_id: EndpointId,
    channels: usize,
    meter: Arc<RouteMeter>,
    publisher: BusPublisher,
    activity: Arc<AtomicU64>,
    _stream: cpal::Stream,
}

#[derive(Default)]
struct Runtimes {
    captures: HashMap<EndpointId, CaptureRuntime>,
    buses: HashMap<EndpointId, BusRuntime>,
    routes: HashMap<RouteId, AudioRoute>,
}

impl AudioBackend for CpalBackend {
    fn run(
        self: Box<Self>,
        commands: Receiver<BackendCommand>,
        events: Sender<BackendEvent>,
    ) -> Result<(), AudioError> {
        send_event(
            &events,
            BackendEvent::Status {
                status: BackendStatus::Starting,
            },
        );
        // cpal platforms cannot manage virtual devices without drivers or
        // plugins; report it before Running so the UI gates creation.
        send_event(
            &events,
            BackendEvent::Capabilities {
                capabilities: BackendCapabilities {
                    virtual_devices: false,
                    application_sources: application_sources_supported(),
                },
            },
        );

        let host = cpal::default_host();
        let hub = Arc::new(ControlHub::default());
        let mut known = enumerate_endpoints(&host);
        for endpoint in known.values() {
            send_event(
                &events,
                BackendEvent::EndpointAdded {
                    endpoint: endpoint.clone(),
                },
            );
        }
        send_event(
            &events,
            BackendEvent::Status {
                status: BackendStatus::Running,
            },
        );

        let mut runtimes = Runtimes::default();
        let mut last_meter = Instant::now();
        let mut last_hotplug = Instant::now();

        loop {
            match commands.recv_timeout(COMMAND_POLL) {
                Ok(command) => {
                    if handle_command(command, &host, &mut known, &mut runtimes, &hub, &events) {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
            if last_hotplug.elapsed() >= HOTPLUG_INTERVAL {
                refresh_endpoints(&host, &mut known, &mut runtimes, &events);
                last_hotplug = Instant::now();
            }
            refresh_route_states(&mut runtimes, &events);
            for runtime in runtimes.captures.values_mut() {
                runtime.publisher.collect_garbage();
            }
            for runtime in runtimes.buses.values_mut() {
                runtime.publisher.collect_garbage();
            }
            if last_meter.elapsed() >= METER_INTERVAL {
                for frame in meter_frames(&known, &runtimes) {
                    let _ = events.try_send(BackendEvent::Meter { frame });
                }
                last_meter = Instant::now();
            }
        }

        drop(runtimes);
        send_event(
            &events,
            BackendEvent::Status {
                status: BackendStatus::Stopped,
            },
        );
        Ok(())
    }
}

fn send_event(events: &Sender<BackendEvent>, event: BackendEvent) {
    let _ = events.send(event);
}

/// Whether this platform exposes per-application capture: Windows via WASAPI
/// process loopback, macOS via process taps when the OS has them (14.2+).
fn application_sources_supported() -> bool {
    #[cfg(target_os = "windows")]
    return true;
    #[cfg(target_os = "macos")]
    return super::macos_tap::taps_available();
}

fn backend_error(user: impl Into<String>, technical: impl Into<String>) -> AudioError {
    AudioError::new(
        ErrorCode::BackendUnavailable,
        ErrorSeverity::Error,
        true,
        user,
        technical,
    )
}

fn route_error(
    code: ErrorCode,
    user: impl Into<String>,
    technical: impl Into<String>,
) -> AudioError {
    AudioError::new(code, ErrorSeverity::Error, true, user, technical)
}

/// Enumerate the host's devices into Orion endpoints, one per direction per
/// device. Endpoint IDs derive from cpal's stable device IDs, so persisted
/// routes survive restarts and hot-plug.
fn enumerate_endpoints(host: &cpal::Host) -> HashMap<EndpointId, AudioEndpoint> {
    let default_input = host
        .default_input_device()
        .and_then(|device| device.id().ok())
        .map(|id| id.to_string());
    let default_output = host
        .default_output_device()
        .and_then(|device| device.id().ok())
        .map(|id| id.to_string());

    let mut endpoints = HashMap::new();
    let Ok(devices) = host.devices() else {
        return endpoints;
    };
    for device in devices {
        let device_id = device.id().ok().map(|id| id.to_string());
        let name = device.to_string();
        for (input, endpoint_type, config) in [
            (
                true,
                EndpointType::PhysicalInput,
                device.default_input_config().ok(),
            ),
            (
                false,
                EndpointType::PhysicalOutput,
                device.default_output_config().ok(),
            ),
        ] {
            let Some(config) = config else { continue };
            let mut identity = EndpointIdentity::new("cpal");
            identity.serial = device_id.clone();
            identity.device_name = Some(name.clone());
            let endpoint_id = stable_endpoint_id(&identity, endpoint_type);
            let channel_count = u32::from(config.channels());
            let channels = (0..channel_count)
                .map(|index| stable_channel_id(endpoint_id, index))
                .collect();
            let default = if input {
                default_input.as_ref()
            } else {
                default_output.as_ref()
            };
            let is_default = device_id.as_ref() == default;
            endpoints.insert(
                endpoint_id,
                AudioEndpoint {
                    id: endpoint_id,
                    runtime_id: None,
                    device_id: stable_device_id(&identity),
                    virtual_device_id: None,
                    identity,
                    name: name.clone(),
                    description: name.clone(),
                    endpoint_type,
                    state: EndpointState::Available,
                    channel_count,
                    sample_rate: Some(config.sample_rate()),
                    is_default,
                    channels,
                    gain: GainDb::default(),
                    muted: false,
                    balance: NormalizedBalance::default(),
                },
            );
        }
    }
    // Windows: each process with a live audio session becomes a routeable
    // application source (captured via WASAPI process loopback on connect).
    #[cfg(target_os = "windows")]
    for endpoint in super::windows_loopback::enumerate_application_sessions() {
        endpoints.insert(endpoint.id, endpoint);
    }
    // macOS: each process producing audio right now becomes a routeable
    // application source (captured via a Core Audio process tap on connect).
    #[cfg(target_os = "macos")]
    for endpoint in super::macos_tap::enumerate_application_processes() {
        endpoints.insert(endpoint.id, endpoint);
    }
    endpoints
}

/// Poll-based hotplug: cpal has no cross-platform device events, so the
/// backend re-enumerates and diffs by the stable endpoint ID.
fn refresh_endpoints(
    host: &cpal::Host,
    known: &mut HashMap<EndpointId, AudioEndpoint>,
    runtimes: &mut Runtimes,
    events: &Sender<BackendEvent>,
) {
    let current = enumerate_endpoints(host);
    let removed: Vec<EndpointId> = known
        .keys()
        .filter(|id| !current.contains_key(*id))
        .copied()
        .collect();
    for endpoint_id in removed {
        known.remove(&endpoint_id);
        // Routes touching a vanished device are torn down with their streams.
        let affected: Vec<RouteId> = runtimes
            .routes
            .values()
            .filter(|route| route.source == endpoint_id || route.destination == endpoint_id)
            .map(|route| route.id)
            .collect();
        for route_id in affected {
            unlink_route(runtimes, route_id);
            runtimes.routes.remove(&route_id);
            send_event(events, BackendEvent::RouteRemoved { route_id });
        }
        drop_runtime_if_orphaned(runtimes, endpoint_id, true);
        drop_runtime_if_orphaned(runtimes, endpoint_id, false);
        send_event(events, BackendEvent::EndpointRemoved { endpoint_id });
    }
    for (endpoint_id, endpoint) in current {
        match known.get(&endpoint_id) {
            None => {
                known.insert(endpoint_id, endpoint.clone());
                send_event(events, BackendEvent::EndpointAdded { endpoint });
            }
            Some(existing) if existing.is_default != endpoint.is_default => {
                let mut updated = existing.clone();
                updated.is_default = endpoint.is_default;
                known.insert(endpoint_id, updated.clone());
                send_event(events, BackendEvent::EndpointUpdated { endpoint: updated });
            }
            Some(_) => {}
        }
    }
}

/// Negotiate an f32 stream config for the endpoint: the engine's requested
/// rate when the device supports it (WASAPI/CoreAudio convert at the OS
/// boundary), otherwise the device's own default rate. Nominal-rate
/// conversion between mismatched endpoints is the platform's job; the engine
/// only corrects clock drift.
fn stream_config(
    device: &cpal::Device,
    input: bool,
    hub: &ControlHub,
) -> Result<(cpal::StreamConfig, usize, u32), AudioError> {
    let default = if input {
        device.default_input_config()
    } else {
        device.default_output_config()
    }
    .map_err(|error| {
        backend_error(
            "The selected device has no default audio configuration.",
            format!("default config query failed: {error}"),
        )
    })?;
    let channels = usize::from(default.channels());
    if channels == 0 {
        return Err(backend_error(
            "The selected device exposes no audio channels.",
            "device reports zero channels".to_string(),
        ));
    }
    let desired = hub.stream_rate();
    let rate_supported = if input {
        device
            .supported_input_configs()
            .map(|ranges| rate_in_ranges(ranges, channels, desired))
            .unwrap_or(false)
    } else {
        device
            .supported_output_configs()
            .map(|ranges| rate_in_ranges(ranges, channels, desired))
            .unwrap_or(false)
    };
    let rate = if rate_supported {
        desired
    } else {
        default.sample_rate()
    };
    let config = cpal::StreamConfig {
        channels: channels as u16,
        sample_rate: rate,
        buffer_size: cpal::BufferSize::Fixed(hub.buffer_frames()),
    };
    Ok((config, channels, rate))
}

/// Whether any supported f32 config range covers the requested rate at the
/// negotiated channel count.
fn rate_in_ranges(
    ranges: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
    channels: usize,
    desired: u32,
) -> bool {
    ranges.into_iter().any(|range| {
        range.channels() as usize == channels
            && range.sample_format() == cpal::SampleFormat::F32
            && range.min_sample_rate() <= desired
            && desired <= range.max_sample_rate()
    })
}

/// Find the host device whose derived endpoint ID matches, in the requested
/// direction (input and output endpoints of one duplex device differ).
fn find_device(
    host: &cpal::Host,
    endpoint_id: EndpointId,
    input: bool,
) -> Result<cpal::Device, AudioError> {
    let devices = host.devices().map_err(|error| {
        backend_error(
            "The system's audio devices could not be listed.",
            format!("device enumeration failed: {error}"),
        )
    })?;
    for device in devices {
        let mut identity = EndpointIdentity::new("cpal");
        identity.serial = device.id().ok().map(|id| id.to_string());
        identity.device_name = Some(device.to_string());
        let endpoint_type = if input {
            EndpointType::PhysicalInput
        } else {
            EndpointType::PhysicalOutput
        };
        if stable_endpoint_id(&identity, endpoint_type) == endpoint_id {
            return Ok(device);
        }
    }
    Err(route_error(
        ErrorCode::EndpointNotFound,
        "That device is no longer available.",
        format!("endpoint {endpoint_id} not found in cpal enumeration"),
    ))
}

fn open_capture(
    host: &cpal::Host,
    endpoint: &AudioEndpoint,
    hub: &Arc<ControlHub>,
) -> Result<CaptureRuntime, AudioError> {
    // Application sources capture through WASAPI process loopback instead
    // of a device stream (Windows only; macOS gains taps separately).
    #[cfg(target_os = "windows")]
    if endpoint.endpoint_type == EndpointType::ApplicationOutput {
        let (capture, publisher, meter) =
            super::windows_loopback::LoopbackCapture::start(endpoint, hub)?;
        let activity = capture.activity().clone();
        return Ok(CaptureRuntime {
            endpoint_id: endpoint.id,
            meter,
            publisher,
            activity,
            _driver: CaptureDriver::Loopback(capture),
        });
    }
    #[cfg(target_os = "macos")]
    if endpoint.endpoint_type == EndpointType::ApplicationOutput {
        let (capture, publisher, meter) = super::macos_tap::TapCapture::start(endpoint, hub)?;
        let activity = capture.activity().clone();
        return Ok(CaptureRuntime {
            endpoint_id: endpoint.id,
            meter,
            publisher,
            activity,
            _driver: CaptureDriver::Tap(capture),
        });
    }
    let device = find_device(host, endpoint.id, true)?;
    let (config, channels, rate) = stream_config(&device, true, hub)?;
    let meter = Arc::new(RouteMeter::new(channels));
    let activity = Arc::new(AtomicU64::new(0));
    // Some devices reject a fixed buffer size; retry with Default once.
    let mut last_error = None;
    for buffer_size in [
        cpal::BufferSize::Fixed(hub.buffer_frames()),
        cpal::BufferSize::Default,
    ] {
        let mut config = config;
        config.buffer_size = buffer_size;
        let (engine, handle) = SourceEngine::new(
            channels,
            rate,
            hub.endpoint_seeded(endpoint),
            hub.channel(endpoint.id),
            meter.clone(),
            Arc::new(PlanSlot::new(SourcePlan {
                generation: 0,
                feeds: Vec::new(),
            })),
        )
        .map_err(|error| {
            route_error(
                ErrorCode::InvalidRoute,
                "Orion could not start realtime processing for that connection.",
                format!("failed to initialize source DSP: {error}"),
            )
        })?;
        let activity_callback = activity.clone();
        let name = endpoint.name.clone();
        let mut engine = engine;
        match device.build_input_stream::<f32, _, _>(
            config,
            move |data, _| {
                activity_callback.fetch_add(1, Ordering::Relaxed);
                engine.process(data);
            },
            move |error| {
                log::warn!("capture stream error on {name}: {error}");
            },
            None,
        ) {
            Ok(stream) => {
                stream.play().map_err(|error| {
                    route_error(
                        ErrorCode::InvalidRoute,
                        "Orion could not start the input stream.",
                        format!(
                            "failed to start capture stream for {}: {error}",
                            endpoint.id
                        ),
                    )
                })?;
                return Ok(CaptureRuntime {
                    endpoint_id: endpoint.id,
                    meter,
                    publisher: SourcePublisher::new(handle),
                    activity,
                    _driver: CaptureDriver::Device(stream),
                });
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(route_error(
        ErrorCode::InvalidRoute,
        "Orion could not open the input device.",
        format!(
            "failed to build capture stream for {}: {:?}",
            endpoint.id, last_error
        ),
    ))
}

fn open_bus(
    host: &cpal::Host,
    endpoint: &AudioEndpoint,
    hub: &Arc<ControlHub>,
) -> Result<BusRuntime, AudioError> {
    let device = find_device(host, endpoint.id, false)?;
    let (config, channels, rate) = stream_config(&device, false, hub)?;
    let meter = Arc::new(RouteMeter::new(channels));
    let activity = Arc::new(AtomicU64::new(0));
    let mut last_error = None;
    for buffer_size in [
        cpal::BufferSize::Fixed(hub.buffer_frames()),
        cpal::BufferSize::Default,
    ] {
        let mut config = config;
        config.buffer_size = buffer_size;
        let (engine, handle) = BusEngine::new(
            channels,
            rate,
            hub.endpoint_seeded(endpoint),
            hub.channel(endpoint.id),
            meter.clone(),
            Arc::new(PlanSlot::new(BusPlan {
                generation: 0,
                routes: Vec::new(),
            })),
        )
        .map_err(|error| {
            route_error(
                ErrorCode::InvalidRoute,
                "Orion could not start realtime processing for that connection.",
                format!("failed to initialize bus DSP: {error}"),
            )
        })?;
        let activity_callback = activity.clone();
        let name = endpoint.name.clone();
        let mut engine = engine;
        match device.build_output_stream::<f32, _, _>(
            config,
            move |data, _| {
                activity_callback.fetch_add(1, Ordering::Relaxed);
                engine.process(data);
            },
            move |error| {
                log::warn!("playback stream error on {name}: {error}");
            },
            None,
        ) {
            Ok(stream) => {
                stream.play().map_err(|error| {
                    route_error(
                        ErrorCode::InvalidRoute,
                        "Orion could not start the output stream.",
                        format!(
                            "failed to start playback stream for {}: {error}",
                            endpoint.id
                        ),
                    )
                })?;
                return Ok(BusRuntime {
                    endpoint_id: endpoint.id,
                    channels,
                    meter,
                    publisher: BusPublisher::new(handle),
                    activity,
                    _stream: stream,
                });
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(route_error(
        ErrorCode::InvalidRoute,
        "Orion could not open the output device.",
        format!(
            "failed to build playback stream for {}: {:?}",
            endpoint.id, last_error
        ),
    ))
}

/// Connect a route: ensure both endpoint streams exist, then deliver a
/// fresh ring into both engines. Mirrors the PipeWire backend's semantics.
// Not the entry API: a failed open must leave nothing inserted, and the
// rollback depends on knowing which side was created by this call.
#[allow(clippy::map_entry)]
fn connect_route(
    host: &cpal::Host,
    known: &mut HashMap<EndpointId, AudioEndpoint>,
    runtimes: &mut Runtimes,
    mut route: AudioRoute,
    hub: &Arc<ControlHub>,
) -> Result<AudioRoute, AudioError> {
    if runtimes.routes.contains_key(&route.id)
        || runtimes.routes.values().any(|existing| {
            existing.source == route.source && existing.destination == route.destination
        })
    {
        return Err(route_error(
            ErrorCode::DuplicateRoute,
            "Those devices are already connected.",
            format!("duplicate cpal route for {}", route.id),
        ));
    }
    let source = known.get(&route.source).cloned().ok_or_else(|| {
        route_error(
            ErrorCode::EndpointNotFound,
            "The connection source is no longer available.",
            format!("source endpoint {} not found", route.source),
        )
    })?;
    let destination = known.get(&route.destination).cloned().ok_or_else(|| {
        route_error(
            ErrorCode::EndpointNotFound,
            "The connection destination is no longer available.",
            format!("destination endpoint {} not found", route.destination),
        )
    })?;
    if !source.endpoint_type.can_source() || !destination.endpoint_type.can_receive() {
        return Err(route_error(
            ErrorCode::InvalidRoute,
            "Those devices cannot be connected.",
            format!(
                "invalid route direction from {:?} to {:?}",
                source.endpoint_type, destination.endpoint_type
            ),
        ));
    }

    let capture_created = !runtimes.captures.contains_key(&source.id);
    if capture_created {
        let runtime = open_capture(host, &source, hub)?;
        runtimes.captures.insert(source.id, runtime);
    }
    if !runtimes.buses.contains_key(&destination.id) {
        match open_bus(host, &destination, hub) {
            Ok(runtime) => {
                runtimes.buses.insert(destination.id, runtime);
            }
            Err(error) => {
                drop_runtime_if_orphaned(runtimes, source.id, true);
                return Err(error);
            }
        }
    }

    let bus_channels = runtimes
        .buses
        .get(&destination.id)
        .map(|runtime| runtime.channels)
        .unwrap_or(2);
    let link = RouteLink::new(
        route.id,
        bus_channels,
        hub.buffer_frames(),
        TARGET_QUANTA,
        hub.stream_rate(),
    )
    .and_then(|link| {
        let source_generation = runtimes
            .captures
            .get(&source.id)
            .map(|runtime| runtime.publisher.next_generation())
            .unwrap_or(1);
        let bus_generation = runtimes
            .buses
            .get(&destination.id)
            .map(|runtime| runtime.publisher.next_generation())
            .unwrap_or(1);
        let balance = hub.endpoint(source.id).balance_gains();
        link.into_halves(source_generation, bus_generation, balance)
    })
    .map_err(|error| {
        route_error(
            ErrorCode::InvalidRoute,
            "Orion could not start realtime processing for that connection.",
            format!("failed to create route link for {}: {error}", route.id),
        )
    })?;
    let (source_half, bus_half) = link;
    runtimes
        .captures
        .get_mut(&source.id)
        .expect("capture runtime ensured")
        .publisher
        .add_feed(route.id, bus_channels, source_half)
        .map_err(|_| {
            route_error(
                ErrorCode::InvalidRoute,
                "Orion could not update that connection.",
                format!("capture inbox full for endpoint {}", source.id),
            )
        })?;
    let draw_result = runtimes
        .buses
        .get_mut(&destination.id)
        .expect("bus runtime ensured")
        .publisher
        .add_draw(route.id, bus_half);
    if draw_result.is_err() {
        runtimes
            .captures
            .get_mut(&source.id)
            .expect("capture runtime ensured")
            .publisher
            .remove_feed(route.id);
        drop_runtime_if_orphaned(runtimes, source.id, true);
        drop_runtime_if_orphaned(runtimes, destination.id, false);
        return Err(route_error(
            ErrorCode::InvalidRoute,
            "Orion could not update that connection.",
            format!("playback inbox full for endpoint {}", destination.id),
        ));
    }

    route.state = RouteState::Connecting;
    runtimes.routes.insert(route.id, route.clone());
    Ok(route)
}

/// Detach a route from both engines; streams are reference-counted and
/// dropped when their last route leaves (dropping a cpal stream stops it).
fn unlink_route(runtimes: &mut Runtimes, route_id: RouteId) {
    let Some(route) = runtimes.routes.get(&route_id).cloned() else {
        return;
    };
    if let Some(runtime) = runtimes.captures.get_mut(&route.source) {
        runtime.publisher.remove_feed(route_id);
    }
    if let Some(runtime) = runtimes.buses.get_mut(&route.destination) {
        runtime.publisher.remove_draw(route_id);
    }
    drop_runtime_if_orphaned(runtimes, route.source, true);
    drop_runtime_if_orphaned(runtimes, route.destination, false);
}

fn drop_runtime_if_orphaned(runtimes: &mut Runtimes, endpoint_id: EndpointId, capture: bool) {
    if capture {
        if runtimes
            .captures
            .get(&endpoint_id)
            .is_some_and(|runtime| !runtime.publisher.has_routes())
        {
            runtimes.captures.remove(&endpoint_id);
        }
    } else if runtimes
        .buses
        .get(&endpoint_id)
        .is_some_and(|runtime| !runtime.publisher.has_routes())
    {
        runtimes.buses.remove(&endpoint_id);
    }
}

/// Tear down every stream and relink all routes against fresh streams, so a
/// rate or buffer change takes effect immediately.
fn rebuild_all_streams(
    host: &cpal::Host,
    known: &mut HashMap<EndpointId, AudioEndpoint>,
    runtimes: &mut Runtimes,
    hub: &Arc<ControlHub>,
    events: &Sender<BackendEvent>,
) {
    let active: Vec<AudioRoute> = runtimes.routes.values().cloned().collect();
    runtimes.captures.clear();
    runtimes.buses.clear();
    runtimes.routes.clear();
    for route in active {
        match connect_route(host, known, runtimes, route.clone(), hub) {
            Ok(route) => send_event(events, BackendEvent::RouteAdded { route }),
            Err(error) => send_event(events, BackendEvent::Error { error }),
        }
    }
}

/// Routes report Active once both endpoint streams have produced buffers.
fn refresh_route_states(runtimes: &mut Runtimes, events: &Sender<BackendEvent>) {
    let route_ids: Vec<RouteId> = runtimes.routes.keys().copied().collect();
    for route_id in route_ids {
        let Some(route) = runtimes.routes.get(&route_id) else {
            continue;
        };
        if route.state != RouteState::Connecting {
            continue;
        }
        let capture_live = runtimes
            .captures
            .get(&route.source)
            .is_some_and(|runtime| runtime.activity.load(Ordering::Relaxed) > 0);
        let bus_live = runtimes
            .buses
            .get(&route.destination)
            .is_some_and(|runtime| runtime.activity.load(Ordering::Relaxed) > 0);
        if capture_live && bus_live {
            let route = runtimes.routes.get_mut(&route_id).expect("route present");
            route.state = RouteState::Active;
            send_event(
                events,
                BackendEvent::RouteUpdated {
                    route: route.clone(),
                },
            );
        }
    }
}

fn meter_frames(
    known: &HashMap<EndpointId, AudioEndpoint>,
    runtimes: &Runtimes,
) -> Vec<MeterFrame> {
    let mut frames = Vec::with_capacity(runtimes.captures.len() + runtimes.buses.len());
    let mut collect = |endpoint_id: EndpointId, meter: &Arc<RouteMeter>| {
        let Some(endpoint) = known.get(&endpoint_id) else {
            return;
        };
        let levels = endpoint
            .channels
            .iter()
            .copied()
            .zip(meter.readings())
            .map(|(channel, reading)| {
                let peak = MeterLevel::new(reading.peak.clamp(MeterLevel::MIN, MeterLevel::MAX))
                    .unwrap_or_default();
                let rms = MeterLevel::new(reading.rms.clamp(MeterLevel::MIN, MeterLevel::MAX))
                    .unwrap_or_default();
                (
                    channel,
                    crate::domain::ChannelLevels {
                        peak,
                        rms,
                        clipped: reading.clipped,
                    },
                )
            })
            .collect();
        frames.push(MeterFrame {
            endpoint_id,
            sequence: meter.next_sequence(),
            levels,
        });
    };
    for runtime in runtimes.captures.values() {
        collect(runtime.endpoint_id, &runtime.meter);
    }
    for runtime in runtimes.buses.values() {
        collect(runtime.endpoint_id, &runtime.meter);
    }
    frames
}

/// Same idempotence contract as the PipeWire backend: a no-change control
/// command completes silently instead of echoing an endpoint update.
fn update_endpoint_control(
    known: &mut HashMap<EndpointId, AudioEndpoint>,
    hub: &Arc<ControlHub>,
    endpoint_id: EndpointId,
    update: impl FnOnce(&mut AudioEndpoint, &crate::realtime::EndpointControls),
) -> Result<Option<AudioEndpoint>, AudioError> {
    let Some(endpoint) = known.get_mut(&endpoint_id) else {
        return Err(route_error(
            ErrorCode::EndpointNotFound,
            "That device is no longer available.",
            format!("endpoint {endpoint_id} not found in cpal inventory"),
        ));
    };
    let before = (endpoint.gain, endpoint.muted, endpoint.balance);
    let controls = hub.endpoint(endpoint_id);
    update(endpoint, &controls);
    let changed = before != (endpoint.gain, endpoint.muted, endpoint.balance);
    Ok(changed.then(|| endpoint.clone()))
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

fn handle_command(
    command: BackendCommand,
    host: &cpal::Host,
    known: &mut HashMap<EndpointId, AudioEndpoint>,
    runtimes: &mut Runtimes,
    hub: &Arc<ControlHub>,
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
        BackendCommand::Connect { command_id, route } => {
            match connect_route(host, known, runtimes, route, hub) {
                Ok(route) => {
                    send_event(events, BackendEvent::RouteAdded { route });
                    send_event(events, BackendEvent::CommandCompleted { command_id });
                }
                Err(error) => send_event(events, BackendEvent::CommandFailed { command_id, error }),
            }
            false
        }
        BackendCommand::Disconnect {
            command_id,
            route_id,
        } => {
            unlink_route(runtimes, route_id);
            runtimes.routes.remove(&route_id);
            send_event(events, BackendEvent::RouteRemoved { route_id });
            send_event(events, BackendEvent::CommandCompleted { command_id });
            false
        }
        BackendCommand::SetVolume {
            command_id,
            endpoint_id,
            gain,
        } => {
            let result = update_endpoint_control(known, hub, endpoint_id, |endpoint, controls| {
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
            let result = update_endpoint_control(known, hub, endpoint_id, |endpoint, controls| {
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
            let result = update_endpoint_control(known, hub, endpoint_id, |endpoint, controls| {
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
            let changed = hub.set_stream_rate(stream_rate) | hub.set_buffer_frames(buffer_frames);
            if changed {
                rebuild_all_streams(host, known, runtimes, hub, events);
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
        BackendCommand::CreateVirtual { command_id, .. }
        | BackendCommand::DeleteVirtual { command_id, .. } => {
            // The capability report already hides these affordances; a
            // command arriving anyway fails cleanly.
            send_event(
                events,
                BackendEvent::CommandFailed {
                    command_id,
                    error: route_error(
                        ErrorCode::UnsupportedOperation,
                        "Virtual devices are not supported by this audio backend.",
                        "CreateVirtual/DeleteVirtual on the cpal backend".to_string(),
                    ),
                },
            );
            false
        }
    }
}

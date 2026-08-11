#![cfg(target_os = "linux")]

use std::{
    fs,
    process::{Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use orion::{
    app_engine::AudioEngine,
    backend::PipeWireBackend,
    domain::{
        AudioEndpoint, ChannelId, EndpointId, EndpointIdentity, EndpointState, EndpointType,
        EngineCommand, EngineCommandKind, EngineEvent, EngineStatus, GainDb, GraphDelta,
        NormalizedBalance, RouteId, RouteState, VirtualDeviceId,
    },
};

static LIVE_PIPEWIRE: Mutex<()> = Mutex::new(());

#[test]
#[ignore = "requires a live PipeWire session"]
fn discovers_live_pipewire_endpoints() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut running = false;
    let mut endpoints = 0;
    let mut virtual_inputs = 0;
    let mut virtual_outputs = 0;

    while Instant::now() < deadline
        && (!running || endpoints == 0 || virtual_inputs < 1 || virtual_outputs < 1)
    {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Status {
                status: EngineStatus::Running,
            })) => running = true,
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint } => {
                    endpoints += 1;
                    match endpoint.endpoint_type {
                        EndpointType::VirtualInput => virtual_inputs += 1,
                        EndpointType::VirtualOutput => virtual_outputs += 1,
                        _ => {}
                    }
                }
                GraphDelta::EndpointRemoved { .. }
                | GraphDelta::RouteAdded { .. }
                | GraphDelta::RouteUpdated { .. }
                | GraphDelta::RouteRemoved { .. } => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }

    assert!(running, "PipeWire backend did not reach Running");
    assert!(
        endpoints > 0,
        "PipeWire reported no routeable audio endpoints"
    );
    assert!(
        virtual_inputs >= 1,
        "Orion did not create its default virtual input"
    );
    assert!(
        virtual_outputs >= 1,
        "Orion did not create its default virtual output"
    );
    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

#[test]
#[ignore = "requires a live PipeWire session"]
fn stream_tuning_command_reaches_backend_and_completes() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));

    let command = EngineCommand::new(EngineCommandKind::SetStreamTuning {
        stream_rate: 96_000,
        buffer_frames: 256,
    });
    let command_id = command.id;
    handle
        .send(command)
        .unwrap_or_else(|error| panic!("failed to send tuning: {error}"));
    wait_for_command(&handle, command_id);

    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

#[test]
#[ignore = "requires a live PipeWire session"]
fn deletes_extra_virtual_but_keeps_the_last_one() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut default_output: Option<VirtualDeviceId> = None;
    let mut default_output_count = 0usize;
    while Instant::now() < deadline && default_output.is_none() {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint }
                    if endpoint.endpoint_type == EndpointType::VirtualOutput =>
                {
                    default_output_count += 1;
                    default_output = endpoint.virtual_device_id;
                }
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    assert_eq!(
        default_output_count, 1,
        "expected exactly one default virtual output"
    );
    let default_output = default_output.expect("default output id captured above");

    // Add a second virtual output, wait for it, then delete it again.
    let virtual_device_id = VirtualDeviceId::new();
    let create = EngineCommand::new(EngineCommandKind::CreateVirtual {
        virtual_device_id,
        endpoint: Box::new(virtual_output_draft(virtual_device_id)),
    });
    let create_id = create.id;
    handle
        .send(create)
        .unwrap_or_else(|error| panic!("failed to request virtual output: {error}"));
    wait_for_command(&handle, create_id);

    let delete = EngineCommand::new(EngineCommandKind::DeleteVirtual { virtual_device_id });
    let delete_id = delete.id;
    handle
        .send(delete)
        .unwrap_or_else(|error| panic!("failed to request virtual delete: {error}"));
    wait_for_command(&handle, delete_id);

    // The last remaining default output must be protected by the backend.
    let last_delete = EngineCommand::new(EngineCommandKind::DeleteVirtual {
        virtual_device_id: default_output,
    });
    let last_delete_id = last_delete.id;
    handle
        .send(last_delete)
        .unwrap_or_else(|error| panic!("failed to request last virtual delete: {error}"));
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut refused = false;
    while Instant::now() < deadline && !refused {
        match handle.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(EngineEvent::Error {
                command_id: Some(id),
                error,
            })) if id == last_delete_id => {
                assert!(
                    error.user_message.contains("at least one"),
                    "unexpected refusal message: {error}"
                );
                refused = true;
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    assert!(refused, "deleting the last virtual output was not refused");
    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

fn virtual_output_draft(virtual_device_id: VirtualDeviceId) -> AudioEndpoint {
    AudioEndpoint {
        id: EndpointId::new(),
        runtime_id: None,
        device_id: None,
        virtual_device_id: Some(virtual_device_id),
        identity: EndpointIdentity::new("orion"),
        name: "Extra Output".into(),
        description: "Extra Output".into(),
        endpoint_type: EndpointType::VirtualOutput,
        state: EndpointState::Available,
        channel_count: 2,
        sample_rate: None,
        is_default: false,
        channels: vec![ChannelId::new(), ChannelId::new()],
        gain: GainDb::default(),
        muted: false,
        balance: NormalizedBalance::default(),
    }
}

#[test]
#[ignore = "requires a live PipeWire session"]
fn connects_and_disconnects_live_route_to_physical_output() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut source = None;
    let mut destination = None;

    while Instant::now() < deadline && (source.is_none() || destination.is_none()) {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint } => match endpoint.endpoint_type {
                    EndpointType::VirtualInput => source = Some(endpoint.id),
                    EndpointType::PhysicalOutput => destination = Some(endpoint.id),
                    _ => {}
                },
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }

    let source = source.unwrap_or_else(|| panic!("no Orion virtual input was discovered"));
    let destination = destination.unwrap_or_else(|| panic!("no physical output was discovered"));
    let route_id = RouteId::new();
    let connect = EngineCommand::new(EngineCommandKind::Connect {
        route_id,
        source,
        destination,
    });
    let connect_id = connect.id;
    handle
        .send(connect)
        .unwrap_or_else(|error| panic!("failed to request route connection: {error}"));

    wait_for_active_route(&handle, route_id, connect_id);

    let gain = GainDb::new(-6.0).unwrap_or_default();
    let set_gain = EngineCommand::new(EngineCommandKind::SetVolume {
        endpoint_id: source,
        gain,
    });
    let set_gain_id = set_gain.id;
    handle
        .send(set_gain)
        .unwrap_or_else(|error| panic!("failed to set route gain: {error}"));
    wait_for_command(&handle, set_gain_id);

    let meter_deadline = Instant::now() + Duration::from_secs(2);
    let mut received_meter = false;
    while Instant::now() < meter_deadline && !received_meter {
        match handle.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(EngineEvent::Meter { frame })) if frame.endpoint_id == source => {
                received_meter = true;
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    assert!(received_meter, "active route did not publish meter frames");

    let disconnect = EngineCommand::new(EngineCommandKind::Disconnect { route_id });
    let disconnect_id = disconnect.id;
    handle
        .send(disconnect)
        .unwrap_or_else(|error| panic!("failed to request route disconnection: {error}"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut removed = false;
    while Instant::now() < deadline && !removed {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => {
                removed =
                    matches!(*delta, GraphDelta::RouteRemoved { route_id: id } if id == route_id);
            }
            Ok(Some(EngineEvent::Error {
                command_id: Some(id),
                error,
            })) if id == disconnect_id => panic!("route disconnection failed: {error}"),
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    assert!(removed, "PipeWire route was not removed");
    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

#[test]
#[ignore = "requires a live PipeWire session"]
fn connects_live_virtual_input_to_virtual_output() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut source = None;
    let mut destination = None;

    while Instant::now() < deadline && (source.is_none() || destination.is_none()) {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint } => match endpoint.endpoint_type {
                    EndpointType::VirtualInput => source.get_or_insert((
                        endpoint.id,
                        endpoint.identity.node_name.unwrap_or_default(),
                    )),
                    EndpointType::VirtualOutput => destination.get_or_insert((
                        endpoint.id,
                        endpoint.identity.node_name.unwrap_or_default(),
                    )),
                    _ => continue,
                },
                _ => continue,
            },
            Ok(Some(_)) | Ok(None) => continue,
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        };
    }

    let (source, source_name) =
        source.unwrap_or_else(|| panic!("no Orion virtual input was discovered"));
    let (destination, destination_name) =
        destination.unwrap_or_else(|| panic!("no Orion virtual output was discovered"));
    let route_id = RouteId::new();
    let connect = EngineCommand::new(EngineCommandKind::Connect {
        route_id,
        source,
        destination,
    });
    let connect_id = connect.id;
    handle
        .send(connect)
        .unwrap_or_else(|error| panic!("failed to request virtual route: {error}"));

    wait_for_active_route(&handle, route_id, connect_id);
    assert_meter_tracks_tone(&handle, source, &source_name);
    assert_virtual_audio_transport(&source_name, &destination_name);
    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

/// While a tone plays into the virtual input, the route must publish meter
/// frames for the source with a non-silent level.
fn assert_meter_tracks_tone(
    handle: &orion::app_engine::EngineHandle,
    source: orion::domain::EndpointId,
    source_name: &str,
) {
    let token = RouteId::new();
    let tone_path = std::path::Path::new("/tmp/opencode").join(format!("orion-{token}-meter.f32"));
    let frames = 48_000_usize;
    let mut input = Vec::with_capacity(frames * 2 * std::mem::size_of::<f32>());
    for frame in 0..frames {
        let sample = (std::f32::consts::TAU * 440.0 * frame as f32 / 48_000.0).sin() * 0.25;
        input.extend_from_slice(&sample.to_le_bytes());
        input.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(&tone_path, input)
        .unwrap_or_else(|error| panic!("failed to write meter test tone: {error}"));

    let mut playback = Command::new("pw-cat")
        .args([
            "--playback",
            "--raw",
            "--format",
            "f32",
            "--rate",
            "48000",
            "--channels",
            "2",
            "--target",
            source_name,
        ])
        .arg(&tone_path)
        .spawn()
        .unwrap_or_else(|error| panic!("failed to play meter test tone: {error}"));

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut heard = false;
    while Instant::now() < deadline && !heard {
        match handle.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(EngineEvent::Meter { frame })) if frame.endpoint_id == source => {
                heard = frame.levels.values().any(|level| level.value() > 0.01);
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    let _ = playback.wait();
    let _ = fs::remove_file(tone_path);
    assert!(heard, "route meters did not track the playing tone");
}

fn assert_virtual_audio_transport(source_name: &str, destination_name: &str) {
    assert!(
        !source_name.is_empty(),
        "virtual input has no PipeWire node name"
    );
    assert!(
        !destination_name.is_empty(),
        "virtual output has no PipeWire node name"
    );
    let token = RouteId::new();
    let temp_dir = std::path::Path::new("/tmp/opencode");
    let input_path = temp_dir.join(format!("orion-{token}-input.f32"));
    let output_path = temp_dir.join(format!("orion-{token}-output.f32"));
    let frames = 48_000_usize;
    let mut input = Vec::with_capacity(frames * 2 * std::mem::size_of::<f32>());
    for frame in 0..frames {
        let sample = (std::f32::consts::TAU * 440.0 * frame as f32 / 48_000.0).sin() * 0.25;
        input.extend_from_slice(&sample.to_le_bytes());
        input.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(&input_path, input)
        .unwrap_or_else(|error| panic!("failed to write virtual route test tone: {error}"));

    let recorder = Command::new("pw-cat")
        .args([
            "--record",
            "--raw",
            "--format",
            "f32",
            "--rate",
            "48000",
            "--channels",
            "2",
            "--sample-count",
            "96000",
            "--target",
            destination_name,
        ])
        .arg(&output_path)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("failed to start virtual output recorder: {error}"));
    thread::sleep(Duration::from_millis(250));
    let playback = Command::new("pw-cat")
        .args([
            "--playback",
            "--raw",
            "--format",
            "f32",
            "--rate",
            "48000",
            "--channels",
            "2",
            "--target",
            source_name,
        ])
        .arg(&input_path)
        .status()
        .unwrap_or_else(|error| panic!("failed to play virtual route test tone: {error}"));
    assert!(playback.success(), "virtual route tone playback failed");
    let recorded = recorder
        .wait_with_output()
        .unwrap_or_else(|error| panic!("failed to wait for virtual output recorder: {error}"));
    let output = fs::read(&output_path)
        .unwrap_or_else(|error| panic!("failed to read virtual output recording: {error}"));
    let transported = output
        .chunks_exact(4)
        .any(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).abs() > 0.01);
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
    assert!(
        transported,
        "virtual route recording contained only silence (recorder {}: {})",
        recorded.status,
        String::from_utf8_lossy(&recorded.stderr)
    );
}

fn wait_for_command(
    handle: &orion::app_engine::EngineHandle,
    command_id: orion::domain::CommandId,
) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match handle.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(EngineEvent::CommandCompleted { command_id: id })) if id == command_id => {
                return;
            }
            Ok(Some(EngineEvent::Error {
                command_id: Some(id),
                error,
            })) if id == command_id => panic!("backend command failed: {error}"),
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    panic!("PipeWire backend command did not complete");
}

fn wait_for_active_route(
    handle: &orion::app_engine::EngineHandle,
    route_id: RouteId,
    command_id: orion::domain::CommandId,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::RouteAdded { route } | GraphDelta::RouteUpdated { route }
                    if route.id == route_id && route.state == RouteState::Active =>
                {
                    return;
                }
                GraphDelta::RouteUpdated { route }
                    if route.id == route_id && route.state == RouteState::Failed =>
                {
                    panic!("PipeWire route entered the failed state");
                }
                _ => {}
            },
            Ok(Some(EngineEvent::Error {
                command_id: Some(id),
                error,
            })) if id == command_id => panic!("route connection failed: {error}"),
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    panic!("PipeWire route did not become active");
}

#[test]
#[ignore = "requires a live PipeWire session"]
fn meters_follow_route_reconnects() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));

    // Dedicated virtual endpoints (unique names) so a running Orion instance
    // and its default routes cannot pollute the assertions.
    let input_id = VirtualDeviceId::new();
    let output_id = VirtualDeviceId::new();
    let tag = &input_id.to_string()[..8];
    handle
        .send(EngineCommand::new(EngineCommandKind::CreateVirtual {
            virtual_device_id: input_id,
            endpoint: Box::new(test_endpoint(
                input_id,
                EndpointType::VirtualInput,
                &format!("Orion Test In {tag}"),
            )),
        }))
        .expect("create input");
    handle
        .send(EngineCommand::new(EngineCommandKind::CreateVirtual {
            virtual_device_id: output_id,
            endpoint: Box::new(test_endpoint(
                output_id,
                EndpointType::VirtualOutput,
                &format!("Orion Test Out {tag}"),
            )),
        }))
        .expect("create output");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut source = None;
    let mut destination = None;
    while Instant::now() < deadline && (source.is_none() || destination.is_none()) {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint } => {
                    if endpoint.virtual_device_id == Some(input_id) {
                        source = Some(endpoint.id);
                    }
                    if endpoint.virtual_device_id == Some(output_id) {
                        destination = Some(endpoint.id);
                    }
                }
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    let (source, destination) = (
        source.expect("test input registered"),
        destination.expect("test output registered"),
    );

    let route_id = RouteId::new();
    handle
        .send(EngineCommand::new(EngineCommandKind::Connect {
            route_id,
            source,
            destination,
        }))
        .expect("connect");
    let metered = collect_meter_endpoints(&handle, Duration::from_secs(3));
    assert!(
        metered.contains(&source) && metered.contains(&destination),
        "meters should flow for the connected route, got {metered:?}"
    );

    handle
        .send(EngineCommand::new(EngineCommandKind::Disconnect {
            route_id,
        }))
        .expect("disconnect");
    let route_id = RouteId::new();
    handle
        .send(EngineCommand::new(EngineCommandKind::Connect {
            route_id,
            source,
            destination,
        }))
        .expect("reconnect");
    let metered = collect_meter_endpoints(&handle, Duration::from_secs(3));
    assert!(
        metered.contains(&source) && metered.contains(&destination),
        "meters should keep flowing after reconnect, got {metered:?}"
    );

    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

fn test_endpoint(
    virtual_device_id: VirtualDeviceId,
    endpoint_type: EndpointType,
    name: &str,
) -> AudioEndpoint {
    AudioEndpoint {
        id: EndpointId::new(),
        runtime_id: None,
        device_id: None,
        virtual_device_id: Some(virtual_device_id),
        identity: EndpointIdentity::new("orion"),
        description: name.to_string(),
        name: name.to_string(),
        endpoint_type,
        state: EndpointState::Available,
        channel_count: 2,
        sample_rate: Some(48_000),
        is_default: false,
        channels: vec![ChannelId::new(), ChannelId::new()],
        gain: GainDb::default(),
        muted: false,
        balance: NormalizedBalance::default(),
    }
}

/// Collect the endpoint ids that report meter frames within `window`.
fn collect_meter_endpoints(
    handle: &orion::app_engine::EngineHandle,
    window: Duration,
) -> std::collections::HashSet<EndpointId> {
    let deadline = Instant::now() + window;
    let mut endpoints = std::collections::HashSet::new();
    while Instant::now() < deadline {
        match handle.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(EngineEvent::Meter { frame })) => {
                endpoints.insert(frame.endpoint_id);
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    endpoints
}

#[test]
#[ignore = "requires a live PipeWire session with a physical output"]
fn meters_flow_to_physical_output_after_connect() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));

    // Wait for a physical output + the default virtual input.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut virtual_input: Option<EndpointId> = None;
    let mut physical_output: Option<EndpointId> = None;
    while Instant::now() < deadline && (virtual_input.is_none() || physical_output.is_none()) {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint } => match endpoint.endpoint_type {
                    EndpointType::VirtualInput => virtual_input = Some(endpoint.id),
                    EndpointType::PhysicalOutput => physical_output = Some(endpoint.id),
                    _ => {}
                },
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    let (source, destination) = (
        virtual_input.expect("virtual input present"),
        physical_output.expect("a physical output must exist for this test"),
    );

    let route_id = RouteId::new();
    handle
        .send(EngineCommand::new(EngineCommandKind::Connect {
            route_id,
            source,
            destination,
        }))
        .expect("connect");

    // Wait for the route to report Active, then collect meters.
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut route_state = None;
    while Instant::now() < deadline && route_state != Some(RouteState::Active) {
        match handle.recv_timeout(Duration::from_millis(200)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::RouteAdded { route } | GraphDelta::RouteUpdated { route }
                    if route.id == route_id =>
                {
                    route_state = Some(route.state);
                }
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    assert_eq!(
        route_state,
        Some(RouteState::Active),
        "route to physical output never reached Active (state: {route_state:?})"
    );

    let metered = collect_meter_endpoints(&handle, Duration::from_secs(2));
    assert!(
        metered.contains(&destination),
        "physical output should meter once the route is active, got {metered:?}"
    );

    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

#[test]
#[ignore = "requires a live PipeWire session with a physical output"]
fn meters_flow_to_suspended_output() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));

    // Find the suspended physical output (the ZEN DAC if present) and the
    // default virtual input.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut virtual_input: Option<EndpointId> = None;
    let mut target: Option<(EndpointId, String)> = None;
    let mut fallback: Option<(EndpointId, String)> = None;
    while Instant::now() < deadline && (virtual_input.is_none() || target.is_none()) {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint } => match endpoint.endpoint_type {
                    EndpointType::VirtualInput => virtual_input = Some(endpoint.id),
                    EndpointType::PhysicalOutput => {
                        let candidate = (endpoint.id, endpoint.name.clone());
                        if endpoint.name.contains("ZEN") {
                            target = Some(candidate);
                        } else if fallback.is_none() {
                            fallback = Some(candidate);
                        }
                    }
                    _ => {}
                },
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    let source = virtual_input.expect("virtual input present");
    let (destination, name) = target.or(fallback).expect("a physical output must exist");
    eprintln!("routing to {name}");

    let route_id = RouteId::new();
    handle
        .send(EngineCommand::new(EngineCommandKind::Connect {
            route_id,
            source,
            destination,
        }))
        .expect("connect");

    let metered = collect_meter_endpoints(&handle, Duration::from_secs(4));
    assert!(
        metered.contains(&destination),
        "suspended output should meter once routed, got {metered:?}"
    );

    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

#[test]
#[ignore = "requires a live PipeWire session"]
fn idempotent_mute_does_not_echo_endpoint_updates() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));

    // Dedicated virtual output so nothing else mutates it during the test.
    let virtual_device_id = VirtualDeviceId::new();
    let tag = &virtual_device_id.to_string()[..8];
    handle
        .send(EngineCommand::new(EngineCommandKind::CreateVirtual {
            virtual_device_id,
            endpoint: Box::new(test_endpoint(
                virtual_device_id,
                EndpointType::VirtualOutput,
                &format!("Orion Mute Test {tag}"),
            )),
        }))
        .expect("create virtual");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut endpoint = None;
    while Instant::now() < deadline && endpoint.is_none() {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint: e }
                | GraphDelta::EndpointUpdated { endpoint: e }
                    if e.virtual_device_id == Some(virtual_device_id) =>
                {
                    endpoint = Some(e.id);
                }
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    let endpoint_id = endpoint.expect("virtual output registered");

    // Mute it, then send the same mute twice more. Exactly one
    // EndpointUpdated (for the real change) may arrive; idempotent repeats
    // must complete silently — otherwise UI re-push loops never settle.
    for _ in 0..3 {
        handle
            .send(EngineCommand::new(EngineCommandKind::SetMute {
                endpoint_id,
                muted: true,
            }))
            .expect("mute");
    }
    // Registry churn (node state flapping) also produces EndpointUpdated;
    // what must not repeat is a MUTE-FIELD flip. Track mute transitions.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last_muted = false;
    let mut mute_flips = 0;
    while Instant::now() < deadline {
        match handle.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointUpdated { endpoint: e }
                    if e.id == endpoint_id && e.muted != last_muted =>
                {
                    mute_flips += 1;
                    last_muted = e.muted;
                }
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    assert_eq!(
        mute_flips, 1,
        "three identical mutes must produce exactly one mute transition, got {mute_flips}"
    );

    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

#[test]
#[ignore = "requires a live PipeWire session with a physical output"]
fn meters_survive_mute_unmute_cycle() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));

    // Default virtual input + the ZEN DAC (or any physical output).
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut virtual_input: Option<EndpointId> = None;
    let mut target: Option<(EndpointId, String)> = None;
    let mut fallback: Option<(EndpointId, String)> = None;
    while Instant::now() < deadline && (virtual_input.is_none() || target.is_none()) {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint } => match endpoint.endpoint_type {
                    EndpointType::VirtualInput => virtual_input = Some(endpoint.id),
                    EndpointType::PhysicalOutput => {
                        let candidate = (endpoint.id, endpoint.name.clone());
                        if endpoint.name.contains("ZEN") {
                            target = Some(candidate);
                        } else if fallback.is_none() {
                            fallback = Some(candidate);
                        }
                    }
                    _ => {}
                },
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    let source = virtual_input.expect("virtual input present");
    let (destination, name) = target.or(fallback).expect("physical output present");
    eprintln!("routing to {name}");

    let route_id = RouteId::new();
    handle
        .send(EngineCommand::new(EngineCommandKind::Connect {
            route_id,
            source,
            destination,
        }))
        .expect("connect");
    let metered = collect_meter_endpoints(&handle, Duration::from_secs(3));
    assert!(
        metered.contains(&destination),
        "meters should flow before mute, got {metered:?}"
    );

    // Mute, then unmute; meters must flow again afterwards.
    for muted in [true, false] {
        handle
            .send(EngineCommand::new(EngineCommandKind::SetMute {
                endpoint_id: destination,
                muted,
            }))
            .expect("mute command");
        thread::sleep(Duration::from_millis(300));
    }

    let metered = collect_meter_endpoints(&handle, Duration::from_secs(3));
    assert!(
        metered.contains(&destination),
        "meters should flow after unmute, got {metered:?}"
    );

    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

#[test]
#[ignore = "requires a live PipeWire session with a physical output"]
fn shared_destination_meters_peak_across_routes() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));

    // Two dedicated virtual inputs + one physical output.
    let input_a = VirtualDeviceId::new();
    let input_b = VirtualDeviceId::new();
    let tag = &input_a.to_string()[..8];
    for (id, name) in [
        (input_a, format!("Orion Share A {tag}")),
        (input_b, format!("Orion Share B {tag}")),
    ] {
        handle
            .send(EngineCommand::new(EngineCommandKind::CreateVirtual {
                virtual_device_id: id,
                endpoint: Box::new(test_endpoint(id, EndpointType::VirtualInput, &name)),
            }))
            .expect("create virtual input");
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut source_a = None;
    let mut source_b = None;
    let mut destination = None;
    while Instant::now() < deadline
        && (source_a.is_none() || source_b.is_none() || destination.is_none())
    {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint } => {
                    if endpoint.virtual_device_id == Some(input_a) {
                        source_a = Some(endpoint.id);
                    } else if endpoint.virtual_device_id == Some(input_b) {
                        source_b = Some(endpoint.id);
                    } else if endpoint.endpoint_type == EndpointType::PhysicalOutput
                        && destination.is_none()
                    {
                        destination = Some(endpoint.id);
                    }
                }
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    let (source_a, source_b, destination) = (
        source_a.expect("input A"),
        source_b.expect("input B"),
        destination.expect("physical output"),
    );

    for source in [source_a, source_b] {
        handle
            .send(EngineCommand::new(EngineCommandKind::Connect {
                route_id: RouteId::new(),
                source,
                destination,
            }))
            .expect("connect");
    }

    // Mute source B at the endpoint: its route into the destination goes
    // silent. The destination must keep metering (A's frames) regardless.
    handle
        .send(EngineCommand::new(EngineCommandKind::SetMute {
            endpoint_id: source_b,
            muted: true,
        }))
        .expect("mute B");
    thread::sleep(Duration::from_millis(300));

    // One combined frame per endpoint per interval: with two routes the old
    // code emitted ~2x the interval rate, last-writer-wins.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut frames = 0usize;
    while Instant::now() < deadline {
        match handle.recv_timeout(Duration::from_millis(50)) {
            Ok(Some(EngineEvent::Meter { frame })) if frame.endpoint_id == destination => {
                frames += 1;
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    assert!(
        frames > 20,
        "destination should meter continuously: {frames}"
    );
    // Combined: ~one frame per 33ms interval (~60 per 2s window with jitter);
    // uncombined two routes would emit twice that (~120).
    assert!(
        frames < 90,
        "frames must be combined per endpoint (one per interval), got {frames} in 2s"
    );

    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

/// Peak level reported for an endpoint over the window.
fn collect_meter_level(
    handle: &orion::app_engine::EngineHandle,
    endpoint_id: EndpointId,
    window: Duration,
) -> f32 {
    let deadline = Instant::now() + window;
    let mut peak = 0.0f32;
    while Instant::now() < deadline {
        match handle.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(EngineEvent::Meter { frame })) if frame.endpoint_id == endpoint_id => {
                for level in frame.levels.values() {
                    peak = peak.max(f32::from(*level));
                }
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    peak
}

#[test]
#[ignore = "requires a live PipeWire session with a physical output"]
fn muting_destination_does_not_silence_source_meter() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));

    // Dedicated virtual input + a physical output.
    let input_id = VirtualDeviceId::new();
    let tag = &input_id.to_string()[..8];
    handle
        .send(EngineCommand::new(EngineCommandKind::CreateVirtual {
            virtual_device_id: input_id,
            endpoint: Box::new(test_endpoint(
                input_id,
                EndpointType::VirtualInput,
                &format!("Orion Meter Src {tag}"),
            )),
        }))
        .expect("create input");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut source = None;
    let mut source_runtime = None;
    let mut destination = None;
    while Instant::now() < deadline && (source.is_none() || destination.is_none()) {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint } => {
                    if endpoint.virtual_device_id == Some(input_id) {
                        source = Some(endpoint.id);
                        source_runtime = endpoint.runtime_id;
                    } else if endpoint.endpoint_type == EndpointType::PhysicalOutput
                        && destination.is_none()
                    {
                        destination = Some(endpoint.id);
                    }
                }
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    let (source, destination) = (
        source.expect("virtual input registered"),
        destination.expect("physical output present"),
    );
    let _ = source_runtime;

    // Play a sine into the virtual input from a helper process (target by
    // node name; pw-cat resolves names, not runtime ids). The file is
    // generated in-test so the case has no external dependency.
    let sine_path = std::env::temp_dir().join(format!("orion-test-sine-{tag}.wav"));
    write_test_sine(&sine_path);
    let mut player = Command::new("pw-cat")
        .args([
            "--playback",
            "--target",
            &format!("orion.virtual-input.{input_id}"),
        ])
        .arg(&sine_path)
        .spawn()
        .expect("pw-cat available");

    let route_id = RouteId::new();
    handle
        .send(EngineCommand::new(EngineCommandKind::Connect {
            route_id,
            source,
            destination,
        }))
        .expect("connect");

    // Wait until the route is actually streaming before reading meters.
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut active = false;
    while Instant::now() < deadline && !active {
        match handle.recv_timeout(Duration::from_millis(200)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::RouteAdded { route } | GraphDelta::RouteUpdated { route }
                    if route.id == route_id =>
                {
                    active = matches!(route.state, RouteState::Active);
                }
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    assert!(active, "route never became active");

    // Source meter is hot with audio playing.
    let level = collect_meter_level(&handle, source, Duration::from_secs(2));

    assert!(level > 0.05, "source should meter the sine, got {level}");

    // Muting the destination silences ITS meter but not the source's.
    handle
        .send(EngineCommand::new(EngineCommandKind::SetMute {
            endpoint_id: destination,
            muted: true,
        }))
        .expect("mute destination");
    thread::sleep(Duration::from_millis(500));
    let source_muted_bus = collect_meter_level(&handle, source, Duration::from_secs(2));
    let destination_muted = collect_meter_level(&handle, destination, Duration::from_secs(1));

    assert!(
        destination_muted < 0.001,
        "muted destination should be silent, got {destination_muted}"
    );
    assert!(
        source_muted_bus > 0.05,
        "destination mute must not silence the source meter, got {source_muted_bus}"
    );

    // Unmute restores the destination meter.
    handle
        .send(EngineCommand::new(EngineCommandKind::SetMute {
            endpoint_id: destination,
            muted: false,
        }))
        .expect("unmute destination");
    let destination_back = collect_meter_level(&handle, destination, Duration::from_secs(2));
    assert!(
        destination_back > 0.05,
        "unmuted destination should meter again, got {destination_back}"
    );

    let _ = player.kill();
    let _ = player.wait();
    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

#[test]
#[ignore = "requires a live PipeWire session with a physical output"]
fn delete_virtual_input_with_active_route_survives() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));

    // Two dedicated virtual inputs (so deletion passes the minimum rule) and
    // a physical output; route A -> output, then delete A.
    let input_a = VirtualDeviceId::new();
    let input_b = VirtualDeviceId::new();
    let tag = &input_a.to_string()[..8];
    for (id, name) in [
        (input_a, format!("Orion Del A {tag}")),
        (input_b, format!("Orion Del B {tag}")),
    ] {
        handle
            .send(EngineCommand::new(EngineCommandKind::CreateVirtual {
                virtual_device_id: id,
                endpoint: Box::new(test_endpoint(id, EndpointType::VirtualInput, &name)),
            }))
            .expect("create virtual input");
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut endpoint_a = None;
    let mut destination = None;
    while Instant::now() < deadline && (endpoint_a.is_none() || destination.is_none()) {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint } => {
                    if endpoint.virtual_device_id == Some(input_a) {
                        endpoint_a = Some(endpoint.id);
                    } else if endpoint.endpoint_type == EndpointType::PhysicalOutput
                        && destination.is_none()
                    {
                        destination = Some(endpoint.id);
                    }
                }
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    let (source, destination) = (endpoint_a.expect("input A"), destination.expect("output"));
    handle
        .send(EngineCommand::new(EngineCommandKind::Connect {
            route_id: RouteId::new(),
            source,
            destination,
        }))
        .expect("connect");
    thread::sleep(Duration::from_millis(500));

    handle
        .send(EngineCommand::new(EngineCommandKind::DeleteVirtual {
            virtual_device_id: input_a,
        }))
        .expect("delete");
    // The engine must stay alive and keep answering commands.
    thread::sleep(Duration::from_millis(800));
    let command = EngineCommand::new(EngineCommandKind::SetStreamTuning {
        stream_rate: 48_000,
        buffer_frames: 512,
    });
    let command_id = command.id;
    handle.send(command).expect("send after delete");
    wait_for_command(&handle, command_id);

    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

#[test]
#[ignore = "requires a live PipeWire session"]
fn create_virtual_input_on_running_engine_survives() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));

    // Let the engine settle, then add a virtual input at runtime.
    thread::sleep(Duration::from_secs(2));
    let virtual_device_id = VirtualDeviceId::new();
    let tag = &virtual_device_id.to_string()[..8];
    let command = EngineCommand::new(EngineCommandKind::CreateVirtual {
        virtual_device_id,
        endpoint: Box::new(test_endpoint(
            virtual_device_id,
            EndpointType::VirtualInput,
            &format!("Orion Runtime Add {tag}"),
        )),
    });
    let command_id = command.id;
    handle.send(command).expect("create");
    wait_for_command(&handle, command_id);

    // And the engine must still answer afterwards.
    thread::sleep(Duration::from_millis(500));
    let command = EngineCommand::new(EngineCommandKind::SetStreamTuning {
        stream_rate: 48_000,
        buffer_frames: 512,
    });
    let command_id = command.id;
    handle.send(command).expect("send after create");
    wait_for_command(&handle, command_id);

    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("PipeWire backend failed to shut down: {error}"));
}

#[test]
#[ignore = "requires a live PipeWire session"]
fn restart_with_overlapping_teardown_stays_online() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // First engine: up with an active route, as the app would have it.
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut running = false;
    let mut virtual_input = None;
    let mut virtual_output = None;
    while Instant::now() < deadline
        && !(running && virtual_input.is_some() && virtual_output.is_some())
    {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Status {
                status: EngineStatus::Running,
            })) => running = true,
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint } => match endpoint.endpoint_type {
                    EndpointType::VirtualInput => virtual_input = Some(endpoint.id),
                    EndpointType::VirtualOutput => virtual_output = Some(endpoint.id),
                    _ => {}
                },
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    handle
        .send(EngineCommand::new(EngineCommandKind::Connect {
            route_id: RouteId::new(),
            source: virtual_input.expect("virtual input"),
            destination: virtual_output.expect("virtual output"),
        }))
        .expect("connect");
    thread::sleep(Duration::from_millis(400));

    // The old UI's restart dropped the engine without joining (detached
    // teardown) and started a new one at once — the overlap left the new
    // engine fighting the old one's dying nodes. The fixed restart shuts
    // down and joins first; simulate that and prove the new engine works.
    drop(handle);
    drop(engine); // detach: what the old restart did
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to restart PipeWire backend: {error}"));

    let deadline = Instant::now() + Duration::from_secs(6);
    let mut running = false;
    while Instant::now() < deadline && !running {
        match handle.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(EngineEvent::Status {
                status: EngineStatus::Running,
            })) => running = true,
            Ok(Some(EngineEvent::Error { error, .. })) => {
                panic!("restarted engine errored: {}", error.technical_message)
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("restarted engine channel died: {error}"),
        }
    }
    assert!(running, "restarted engine never reached Running");

    // And it must carry a route end to end.
    let command = EngineCommand::new(EngineCommandKind::SetStreamTuning {
        stream_rate: 48_000,
        buffer_frames: 512,
    });
    let command_id = command.id;
    handle.send(command).expect("send after restart");
    wait_for_command(&handle, command_id);

    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("restarted backend failed to shut down: {error}"));
}

/// Write an 8-second stereo 440 Hz sine as 16-bit PCM WAV.
fn write_test_sine(path: &std::path::Path) {
    let rate = 48_000u32;
    let frames = rate * 8;
    let mut data = Vec::with_capacity(frames as usize * 4);
    for i in 0..frames {
        let sample = (0.4
            * f32::sin(2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32)
            * 32767.0) as i16;
        data.extend_from_slice(&sample.to_le_bytes());
        data.extend_from_slice(&sample.to_le_bytes());
    }
    let mut file = Vec::new();
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend_from_slice(&16u32.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes()); // PCM
    file.extend_from_slice(&2u16.to_le_bytes()); // stereo
    file.extend_from_slice(&rate.to_le_bytes());
    file.extend_from_slice(&(rate * 4).to_le_bytes()); // byte rate
    file.extend_from_slice(&4u16.to_le_bytes()); // block align
    file.extend_from_slice(&16u16.to_le_bytes()); // bits
    file.extend_from_slice(b"data");
    file.extend_from_slice(&(data.len() as u32).to_le_bytes());
    file.extend_from_slice(&data);
    std::fs::write(path, file).expect("write test sine");
}

#[test]
#[ignore = "requires a live PipeWire session"]
fn meters_survive_engine_restart_with_same_route() {
    let _guard = LIVE_PIPEWIRE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // Engine 1: route the default virtual input to the default virtual
    // output and put a sine on it, as the running app would.
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to start PipeWire backend: {error}"));
    let (source, destination) = wait_for_default_virtuals(&handle);
    let route_id = RouteId::new();
    handle
        .send(EngineCommand::new(EngineCommandKind::Connect {
            route_id,
            source,
            destination,
        }))
        .expect("connect");
    thread::sleep(Duration::from_millis(600));
    let before = collect_meter_endpoints(&handle, Duration::from_secs(1));
    assert!(
        before.contains(&source) && before.contains(&destination),
        "meters should flow before restart, got {before:?}"
    );

    // Sequential restart, exactly what the UI does.
    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("backend failed to shut down: {error}"));
    let (engine, handle) = AudioEngine::start(PipeWireBackend)
        .unwrap_or_else(|error| panic!("failed to restart PipeWire backend: {error}"));
    let (source, destination) = wait_for_default_virtuals(&handle);
    let route_id = RouteId::new();
    handle
        .send(EngineCommand::new(EngineCommandKind::Connect {
            route_id,
            source,
            destination,
        }))
        .expect("reconnect after restart");
    thread::sleep(Duration::from_millis(600));
    let after = collect_meter_endpoints(&handle, Duration::from_secs(2));
    assert!(
        after.contains(&source) && after.contains(&destination),
        "meters must flow after restart + reconnect, got {after:?}"
    );

    engine
        .shutdown()
        .unwrap_or_else(|error| panic!("restarted backend failed to shut down: {error}"));
}

/// Wait for the default virtual input/output endpoints of a fresh engine.
fn wait_for_default_virtuals(handle: &orion::app_engine::EngineHandle) -> (EndpointId, EndpointId) {
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut input = None;
    let mut output = None;
    while Instant::now() < deadline && (input.is_none() || output.is_none()) {
        match handle.recv_timeout(Duration::from_millis(200)) {
            Ok(Some(EngineEvent::Delta { delta })) => match *delta {
                GraphDelta::EndpointAdded { endpoint }
                | GraphDelta::EndpointUpdated { endpoint } => match endpoint.endpoint_type {
                    EndpointType::VirtualInput => input = Some(endpoint.id),
                    EndpointType::VirtualOutput => output = Some(endpoint.id),
                    _ => {}
                },
                _ => {}
            },
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => panic!("PipeWire backend event channel failed: {error}"),
        }
    }
    (
        input.expect("virtual input"),
        output.expect("virtual output"),
    )
}

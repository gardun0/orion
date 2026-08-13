//! Windows application audio sources via WASAPI process loopback.
//!
//! Discovery: every audible process on the system holds at least one audio
//! session on some render device; we enumerate sessions and expose each
//! process as an `ApplicationOutput` endpoint (the platform-neutral
//! application-source model — the same type PipeWire uses for app streams).
//!
//! Capture: routing *from* an application endpoint opens a process loopback
//! client (Windows 10 2004+) targeting its PID, on a dedicated thread that
//! owns all COM objects (WASAPI clients are !Send/!Sync) and drives a
//! `SourceEngine` with the captured blocks — identical to a cpal device
//! capture from the engine's point of view.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::thread::JoinHandle;

use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat};
use windows::core::PWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::domain::{
    stable_channel_id, stable_endpoint_id, AudioEndpoint, AudioError, EndpointIdentity,
    EndpointState, EndpointType, ErrorCode, ErrorSeverity,
};
use crate::realtime::{
    ControlHub, PlanSlot, RouteMeter, SourceEngine, SourcePlan, SourcePublisher,
};

/// Enumerate applications with live audio sessions as routeable source
/// endpoints. One endpoint per process (loopback captures the process tree);
/// the identity key is the session's executable name, so a persisted route
/// reconnects when the same app plays again.
pub fn enumerate_application_sessions() -> Vec<AudioEndpoint> {
    let mut endpoints: Vec<AudioEndpoint> = Vec::new();
    if wasapi::initialize_mta().is_err() {
        return endpoints;
    }
    let Ok(enumerator) = wasapi::DeviceEnumerator::new() else {
        return endpoints;
    };
    let Ok(collection) = enumerator.get_device_collection(&Direction::Render) else {
        return endpoints;
    };
    let mut seen_pids = Vec::<u32>::new();
    let Ok(device_count) = collection.get_nbr_devices() else {
        return endpoints;
    };
    for device_index in 0..device_count {
        let Ok(device) = collection.get_device_at_index(device_index) else {
            continue;
        };
        let Ok(manager) = device.get_iaudiosessionmanager() else {
            continue;
        };
        let Ok(sessions) = manager.get_audiosessionenumerator() else {
            continue;
        };
        let Ok(count) = sessions.get_count() else {
            continue;
        };
        for index in 0..count {
            let Ok(session) = sessions.get_session(index) else {
                continue;
            };
            let Ok(pid) = session.get_process_id() else {
                continue;
            };
            if pid == 0 || seen_pids.contains(&pid) {
                continue;
            }
            seen_pids.push(pid);
            // The executable name is both the friendly label and the stable
            // identity (two instances of one app share an endpoint for now).
            let display = process_name(pid);
            let name = display.clone().unwrap_or_else(|| format!("Process {pid}"));
            let mut identity = EndpointIdentity::new("wasapi");
            // The executable/display name is the stable key; the PID is this
            // session's runtime handle.
            identity.serial = display;
            identity.device_name = Some(name.clone());
            let endpoint_id = stable_endpoint_id(&identity, EndpointType::ApplicationOutput);
            endpoints.push(AudioEndpoint {
                id: endpoint_id,
                runtime_id: Some(pid),
                device_id: None,
                virtual_device_id: None,
                identity,
                name: name.clone(),
                description: name,
                endpoint_type: EndpointType::ApplicationOutput,
                state: EndpointState::Available,
                channel_count: 2,
                sample_rate: None,
                is_default: false,
                channels: (0..2)
                    .map(|index| stable_channel_id(endpoint_id, index))
                    .collect(),
                gain: crate::domain::GainDb::default(),
                muted: false,
                balance: crate::domain::NormalizedBalance::default(),
            });
        }
    }
    endpoints
}

/// A running process-loopback capture. Owns the capture thread; dropping
/// stops and joins it (always on the backend thread, never realtime).
pub struct LoopbackCapture {
    stop: Arc<AtomicBool>,
    activity: Arc<AtomicU64>,
    join: Option<JoinHandle<()>>,
}

impl LoopbackCapture {
    /// Start capturing `pid`'s audio into a fresh SourceEngine. All COM work
    /// happens on the capture thread (WASAPI clients are !Send). Returns the
    /// capture handle, the plan publisher, and the engine's shared meter.
    pub fn start(
        endpoint: &AudioEndpoint,
        hub: &Arc<ControlHub>,
    ) -> Result<(Self, SourcePublisher, Arc<RouteMeter>), AudioError> {
        let pid = endpoint.runtime_id.ok_or_else(|| {
            loopback_error(
                "That application is no longer playing audio.",
                format!("application endpoint {} has no process id", endpoint.id),
            )
        })?;
        let channels = 2usize;
        let rate = hub.stream_rate();
        let meter = Arc::new(RouteMeter::new(channels));
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
            loopback_error(
                "Orion could not start realtime processing for that connection.",
                format!("failed to initialize application capture DSP: {error}"),
            )
        })?;

        let stop = Arc::new(AtomicBool::new(false));
        let activity = Arc::new(AtomicU64::new(0));
        let endpoint_name = endpoint.name.clone();
        let buffer_frames = hub.buffer_frames();
        let join = std::thread::Builder::new()
            .name(format!("orion-loopback-{pid}"))
            .spawn({
                let stop = stop.clone();
                let activity = activity.clone();
                move || capture_thread(pid, rate, buffer_frames, engine, stop, activity)
            })
            .map_err(|error| {
                AudioError::thread_failure(
                    "loopback capture",
                    format!("failed to spawn capture thread for {endpoint_name}: {error}"),
                )
            })?;
        let capture = Self {
            stop,
            activity,
            join: Some(join),
        };
        Ok((capture, SourcePublisher::new(handle), meter))
    }

    pub fn activity(&self) -> &Arc<AtomicU64> {
        &self.activity
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// The capture loop: block on the WASAPI event, drain available packets into
/// the engine, repeat until stopped. Runs entirely on its own thread.
fn capture_thread(
    pid: u32,
    rate: u32,
    buffer_frames: u32,
    mut engine: SourceEngine,
    stop: Arc<AtomicBool>,
    activity: Arc<AtomicU64>,
) {
    if wasapi::initialize_mta().is_err() {
        log::error!("loopback capture for pid {pid}: COM init failed");
        return;
    }
    let result = run_capture(pid, rate, buffer_frames, &mut engine, &stop, &activity);
    if let Err(error) = result {
        log::warn!("loopback capture for pid {pid} ended: {error}");
    }
}

fn run_capture(
    pid: u32,
    rate: u32,
    buffer_frames: u32,
    engine: &mut SourceEngine,
    stop: &Arc<AtomicBool>,
    activity: &Arc<AtomicU64>,
) -> Result<(), wasapi::WasapiError> {
    let mut client = AudioClient::new_application_loopback_client(pid, true)?;
    // Shared mode with autoconvert: Windows converts the session's mix
    // format to the requested f32 stereo at the engine's rate.
    let format = WaveFormat::new(32, 32, &SampleType::Float, rate as usize, 2, None);
    // 100-ns units: buffer_frames at the engine rate, with a 10 ms floor.
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: ((u64::from(buffer_frames) * 10_000_000 / u64::from(rate.max(1)))
            as i64)
            .max(100_000),
    };
    client.initialize_client(&format, &Direction::Capture, &mode)?;
    let event = client.set_get_eventhandle()?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;

    // Both buffers persist; they grow only if WASAPI ever delivers a
    // packet larger than the configured quantum (a warm-up-time event).
    let mut bytes = vec![0u8; (buffer_frames as usize).max(1024) * 2 * 4];
    let mut samples = vec![0.0_f32; (buffer_frames as usize).max(1024) * 2];
    while !stop.load(Ordering::Relaxed) {
        // Wake on new audio or every 100 ms to notice the stop flag.
        let _ = event.wait_for_event(100);
        loop {
            let Some(packet_frames) = capture.get_next_packet_size()? else {
                break;
            };
            let needed_bytes = packet_frames as usize * 2 * 4;
            if bytes.len() < needed_bytes {
                bytes.resize(needed_bytes, 0);
            }
            let (frames, _flags) = capture.read_from_device(&mut bytes[..needed_bytes])?;
            if frames == 0 {
                break;
            }
            let needed_samples = frames as usize * 2;
            if samples.len() < needed_samples {
                samples.resize(needed_samples, 0.0);
            }
            for (slot, chunk) in samples[..needed_samples]
                .iter_mut()
                .zip(bytes[..needed_samples * 4].chunks_exact(4))
            {
                *slot = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            }
            engine.process(&samples[..needed_samples]);
            activity.fetch_add(1, Ordering::Relaxed);
        }
    }
    let _ = client.stop_stream();
    Ok(())
}

fn loopback_error(user: impl Into<String>, technical: impl Into<String>) -> AudioError {
    AudioError::new(
        ErrorCode::InvalidRoute,
        ErrorSeverity::Error,
        true,
        user,
        technical,
    )
}

/// The process's executable name (no extension), for endpoint identity and
/// display. Falls back to None when the process can't be queried.
fn process_name(pid: u32) -> Option<String> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = vec![0u16; 260]; // MAX_PATH
    let mut size = buffer.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };
    let _ = unsafe { CloseHandle(handle) };
    result.ok()?;
    let path = String::from_utf16_lossy(&buffer[..size as usize]);
    path.rsplit(['\\', '/'])
        .next()
        .map(|file| file.trim_end_matches(".exe").to_owned())
        .filter(|name| !name.is_empty())
}

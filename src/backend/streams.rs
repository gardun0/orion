//! PipeWire stream adapter: one capture stream per source endpoint and one
//! playback stream per destination endpoint, each driving the
//! platform-neutral engine from `src/realtime`. The adapter owns native
//! buffer (de)serialization and stream lifecycle; all DSP, routing,
//! drift correction, and plan management live in the engine.
//!
//! Streams are ordinary scheduled streams (the old trigger/async topology
//! was retired with per-route streams): PipeWire schedules each stream on
//! its device's clock and converts rate/format at the node boundaries, and
//! the engine absorbs the remaining clock drift between endpoints.

use std::{
    mem,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use pipewire::{self as pw, spa::pod::Pod};

use crate::domain::{
    AudioEndpoint, AudioError, EndpointId, EndpointType, ErrorCode, ErrorSeverity,
};
use crate::realtime::{
    BusEngine, BusInbox, BusPublisher, ControlHub, RouteMeter, SourceEngine, SourceInbox,
    SourcePublisher, MAX_BLOCK_FRAMES,
};

const SAMPLE_BYTES: usize = mem::size_of::<f32>();
/// Stream states reported to the backend loop.
pub const STREAM_CONNECTING: u32 = 0;
pub const STREAM_CONNECTED: u32 = 1;
pub const STREAM_FAILED: u32 = 2;
pub const STREAM_DISCONNECTED: u32 = 3;

/// `node.latency` value: buffer frames expressed against the stream rate.
fn latency_hint_value(hint_frames: u32, rate: u32) -> String {
    format!("{hint_frames}/{rate}")
}

/// Shared liveness bookkeeping for one stream: the activity counter is
/// bumped by the realtime callback and watched by the backend loop, which
/// nudges paused streams and rebuilds ones that stall mid-session.
struct StreamHealth {
    state: Arc<std::sync::atomic::AtomicU32>,
    activity: Arc<AtomicU64>,
    last_seen: u64,
    stalled_since: Option<Instant>,
    created: Instant,
    last_wake: Instant,
}

impl StreamHealth {
    fn new() -> (Self, Arc<std::sync::atomic::AtomicU32>, Arc<AtomicU64>) {
        let state = Arc::new(std::sync::atomic::AtomicU32::new(STREAM_CONNECTING));
        let activity = Arc::new(AtomicU64::new(0));
        (
            Self {
                state: state.clone(),
                activity: activity.clone(),
                last_seen: 0,
                stalled_since: None,
                created: Instant::now(),
                last_wake: Instant::now(),
            },
            state,
            activity,
        )
    }

    fn state_code(&self) -> u32 {
        self.state.load(Ordering::Relaxed)
    }

    /// Reports Streaming but processes no buffers: the stream stalled
    /// (suspend races, driver hiccups). The caller rebuilds it; attempts
    /// are bounded by the caller.
    fn is_stalled(&mut self) -> bool {
        const STALL_AFTER: Duration = Duration::from_millis(750);
        const GRACE: Duration = Duration::from_millis(1_500);
        if self.created.elapsed() < GRACE {
            return false;
        }
        if self.state_code() != STREAM_CONNECTED {
            self.stalled_since = None;
            return false;
        }
        let activity = self.activity.load(Ordering::Relaxed);
        if activity != self.last_seen {
            self.last_seen = activity;
            self.stalled_since = None;
            return false;
        }
        match self.stalled_since {
            Some(since) => since.elapsed() >= STALL_AFTER,
            None => {
                self.stalled_since = Some(Instant::now());
                false
            }
        }
    }

    /// Re-activate streams that are not Streaming yet. Devices suspend when
    /// idle and transitions can be missed across a device switch, so the
    /// backend loop nudges stuck streams (rate-limited) until they run.
    fn ensure_active(&mut self, stream: &pw::stream::Stream) {
        if self.state_code() == STREAM_CONNECTED {
            return;
        }
        if self.last_wake.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_wake = Instant::now();
        let _ = stream.set_active(true);
    }
}

/// Capture stream runtime for one source endpoint.
pub struct CaptureRuntime<'core> {
    endpoint_id: EndpointId,
    meter: Arc<RouteMeter>,
    health: StreamHealth,
    publisher: SourcePublisher,
    _listener: pw::stream::StreamListener<CaptureData>,
    stream: pw::stream::StreamBox<'core>,
}

/// Playback stream runtime for one destination endpoint (the bus).
pub struct PlaybackRuntime<'core> {
    endpoint_id: EndpointId,
    channels: usize,
    meter: Arc<RouteMeter>,
    health: StreamHealth,
    publisher: BusPublisher,
    _listener: pw::stream::StreamListener<PlaybackData>,
    stream: pw::stream::StreamBox<'core>,
}

struct CaptureData {
    engine: SourceEngine,
    scratch: Vec<f32>,
    channels: usize,
    activity: Arc<AtomicU64>,
}

struct PlaybackData {
    engine: BusEngine,
    scratch: Vec<f32>,
    channels: usize,
    activity: Arc<AtomicU64>,
}

impl<'core> CaptureRuntime<'core> {
    pub fn new(
        core: &'core pw::core::CoreRc,
        endpoint: &AudioEndpoint,
        hub: &Arc<ControlHub>,
    ) -> Result<Self, AudioError> {
        let runtime_id = endpoint.runtime_id.ok_or_else(|| {
            stream_error(
                "The route source is not connected to PipeWire.",
                format!("source endpoint {} has no runtime id", endpoint.id),
            )
        })?;
        let channels = stream_channel_count(endpoint)?;
        let rate = hub.stream_rate();
        let meter = Arc::new(RouteMeter::new(channels));
        let (health, state, activity) = StreamHealth::new();
        let (engine, handle) = SourceEngine::new(
            channels,
            rate,
            hub.endpoint_seeded(endpoint),
            hub.channel(endpoint.id),
            meter.clone(),
            Arc::new(crate::realtime::PlanSlot::new(
                crate::realtime::SourcePlan {
                    generation: 0,
                    feeds: Vec::new(),
                },
            )),
        )
        .map_err(dsp_error)?;

        let stream = pw::stream::StreamBox::new(
            core,
            &format!("orion-capture-{}", endpoint.id),
            capture_properties(endpoint, hub),
        )
        .map_err(|error| {
            stream_error(
                "Orion could not create the capture stream for that connection.",
                format!(
                    "failed to create capture stream for {}: {error}",
                    endpoint.id
                ),
            )
        })?;
        let listener = stream
            .add_local_listener_with_user_data(CaptureData {
                engine,
                scratch: vec![0.0; MAX_BLOCK_FRAMES * channels],
                channels,
                activity,
            })
            .state_changed({
                let state = state.clone();
                move |stream, _, _, stream_state| {
                    wake_paused_stream(stream, &stream_state);
                    state.store(stream_state_code(stream_state), Ordering::Relaxed);
                }
            })
            .process(process_capture)
            .register()
            .map_err(|error| {
                stream_error(
                    "Orion could not watch the capture stream for that connection.",
                    format!(
                        "failed to register capture listener for {}: {error}",
                        endpoint.id
                    ),
                )
            })?;
        connect_stream(
            &stream,
            pw::spa::utils::Direction::Input,
            runtime_id,
            channels,
            rate,
            "capture",
        )?;

        Ok(Self {
            endpoint_id: endpoint.id,
            meter,
            health,
            publisher: SourcePublisher::new(handle),
            _listener: listener,
            stream,
        })
    }

    pub const fn endpoint_id(&self) -> EndpointId {
        self.endpoint_id
    }

    pub fn meter(&self) -> &Arc<RouteMeter> {
        &self.meter
    }

    pub fn stream_state(&self) -> u32 {
        self.health.state_code()
    }

    pub fn is_stalled(&mut self) -> bool {
        self.health.is_stalled()
    }

    pub fn ensure_active(&mut self) {
        self.health.ensure_active(&self.stream);
    }

    /// Publish a plan listing `route_id` and deliver the producing half of
    /// its ring. Plan first, then the inbox: the generation tag lets the
    /// callback resolve whichever order they become visible in.
    pub fn add_feed(
        &mut self,
        route_id: crate::domain::RouteId,
        bus_channels: usize,
        item: SourceInbox,
    ) -> Result<(), AudioError> {
        self.publisher
            .add_feed(route_id, bus_channels, item)
            .map_err(|rtrb::PushError::Full(_)| {
                stream_error(
                    "Orion could not update that connection.",
                    format!("capture inbox full for endpoint {}", self.endpoint_id),
                )
            })
    }

    pub fn remove_feed(&mut self, route_id: crate::domain::RouteId) -> Result<(), AudioError> {
        self.publisher.remove_feed(route_id);
        Ok(())
    }

    pub fn has_routes(&self) -> bool {
        self.publisher.has_routes()
    }

    /// Next plan generation (tags the inbox delivery for this route).
    pub fn next_generation(&self) -> u64 {
        self.publisher.next_generation()
    }

    /// Drain retired ring halves and reclaim plans the callback finished
    /// with; drops happen here, on the backend thread.
    pub fn collect_garbage(&mut self) {
        self.publisher.collect_garbage();
    }

    pub fn disconnect(&self) -> Result<(), AudioError> {
        self.stream.disconnect().map_err(|error| {
            stream_error(
                "Orion could not fully remove that connection.",
                format!("failed to disconnect capture {}: {error}", self.endpoint_id),
            )
        })
    }
}

impl<'core> PlaybackRuntime<'core> {
    pub fn new(
        core: &'core pw::core::CoreRc,
        endpoint: &AudioEndpoint,
        hub: &Arc<ControlHub>,
    ) -> Result<Self, AudioError> {
        let runtime_id = endpoint.runtime_id.ok_or_else(|| {
            stream_error(
                "The route destination is not connected to PipeWire.",
                format!("destination endpoint {} has no runtime id", endpoint.id),
            )
        })?;
        let channels = stream_channel_count(endpoint)?;
        let rate = hub.stream_rate();
        let meter = Arc::new(RouteMeter::new(channels));
        let (health, state, activity) = StreamHealth::new();
        let (engine, handle) = BusEngine::new(
            channels,
            rate,
            hub.endpoint_seeded(endpoint),
            hub.channel(endpoint.id),
            meter.clone(),
            Arc::new(crate::realtime::PlanSlot::new(crate::realtime::BusPlan {
                generation: 0,
                routes: Vec::new(),
            })),
        )
        .map_err(dsp_error)?;

        let stream = pw::stream::StreamBox::new(
            core,
            &format!("orion-playback-{}", endpoint.id),
            playback_properties(endpoint, hub),
        )
        .map_err(|error| {
            stream_error(
                "Orion could not create the playback stream for that connection.",
                format!(
                    "failed to create playback stream for {}: {error}",
                    endpoint.id
                ),
            )
        })?;
        let listener = stream
            .add_local_listener_with_user_data(PlaybackData {
                engine,
                scratch: vec![0.0; MAX_BLOCK_FRAMES * channels],
                channels,
                activity,
            })
            .state_changed({
                let state = state.clone();
                move |stream, _, _, stream_state| {
                    wake_paused_stream(stream, &stream_state);
                    state.store(stream_state_code(stream_state), Ordering::Relaxed);
                }
            })
            .process(process_playback)
            .register()
            .map_err(|error| {
                stream_error(
                    "Orion could not watch the playback stream for that connection.",
                    format!(
                        "failed to register playback listener for {}: {error}",
                        endpoint.id
                    ),
                )
            })?;
        connect_stream(
            &stream,
            pw::spa::utils::Direction::Output,
            runtime_id,
            channels,
            rate,
            "playback",
        )?;

        Ok(Self {
            endpoint_id: endpoint.id,
            channels,
            meter,
            health,
            publisher: BusPublisher::new(handle),
            _listener: listener,
            stream,
        })
    }

    pub const fn endpoint_id(&self) -> EndpointId {
        self.endpoint_id
    }

    pub const fn channels(&self) -> usize {
        self.channels
    }

    pub fn meter(&self) -> &Arc<RouteMeter> {
        &self.meter
    }

    pub fn stream_state(&self) -> u32 {
        self.health.state_code()
    }

    pub fn is_stalled(&mut self) -> bool {
        self.health.is_stalled()
    }

    pub fn ensure_active(&mut self) {
        self.health.ensure_active(&self.stream);
    }

    pub fn add_draw(
        &mut self,
        route_id: crate::domain::RouteId,
        item: BusInbox,
    ) -> Result<(), AudioError> {
        self.publisher
            .add_draw(route_id, item)
            .map_err(|rtrb::PushError::Full(_)| {
                stream_error(
                    "Orion could not update that connection.",
                    format!("playback inbox full for endpoint {}", self.endpoint_id),
                )
            })
    }

    pub fn remove_draw(&mut self, route_id: crate::domain::RouteId) -> Result<(), AudioError> {
        self.publisher.remove_draw(route_id);
        Ok(())
    }

    pub fn has_routes(&self) -> bool {
        self.publisher.has_routes()
    }

    pub fn next_generation(&self) -> u64 {
        self.publisher.next_generation()
    }

    pub fn collect_garbage(&mut self) {
        self.publisher.collect_garbage();
    }

    pub fn disconnect(&self) -> Result<(), AudioError> {
        self.stream.disconnect().map_err(|error| {
            stream_error(
                "Orion could not fully remove that connection.",
                format!(
                    "failed to disconnect playback {}: {error}",
                    self.endpoint_id
                ),
            )
        })
    }
}

fn process_capture(stream: &pw::stream::Stream, data: &mut CaptureData) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    data.activity.fetch_add(1, Ordering::Relaxed);
    let Some(buffer_data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let offset = buffer_data.chunk().offset() as usize;
    let size = buffer_data.chunk().size() as usize;
    let chunk_stride = buffer_data.chunk().stride();
    let Some(bytes) = buffer_data.data() else {
        return;
    };
    let start = offset.min(bytes.len());
    let end = start.saturating_add(size).min(bytes.len());
    let bytes = &bytes[start..end];
    let channels = data.channels;
    let frame_bytes = channels.saturating_mul(SAMPLE_BYTES);
    if frame_bytes == 0 {
        return;
    }
    // Honor the driver's frame stride when it reports one (some devices pad
    // frames); falling back to tight packing keeps the default path intact.
    let frame_stride = if chunk_stride > 0 && chunk_stride as usize >= frame_bytes {
        chunk_stride as usize
    } else {
        frame_bytes
    };

    let total_frames = bytes.len() / frame_stride;
    let mut first_frame = 0;
    while first_frame < total_frames {
        let chunk_frames = (total_frames - first_frame).min(MAX_BLOCK_FRAMES);
        let samples = chunk_frames * channels;
        {
            let scratch = &mut data.scratch[..samples];
            for (index, sample) in scratch.iter_mut().enumerate() {
                let frame = first_frame + index / channels;
                let channel = index % channels;
                *sample = read_sample(bytes, frame, channel, frame_stride);
            }
            data.engine.process(scratch);
        }
        first_frame += chunk_frames;
    }
}

fn process_playback(stream: &pw::stream::Stream, data: &mut PlaybackData) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    data.activity.fetch_add(1, Ordering::Relaxed);
    let Some(buffer_data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let Some(bytes) = buffer_data.data() else {
        return;
    };
    let channels = data.channels;
    let frame_bytes = channels.saturating_mul(SAMPLE_BYTES);
    let total_frames = bytes.len().checked_div(frame_bytes).unwrap_or(0);
    if total_frames == 0 {
        return;
    }

    let mut first_frame = 0;
    while first_frame < total_frames {
        let chunk_frames = (total_frames - first_frame).min(MAX_BLOCK_FRAMES);
        let samples = chunk_frames * channels;
        {
            let scratch = &mut data.scratch[..samples];
            data.engine.process(scratch);
            for (index, sample) in scratch.iter().enumerate() {
                let frame = first_frame + index / channels;
                let channel = index % channels;
                write_sample(bytes, frame, channel, frame_bytes, *sample);
            }
        }
        first_frame += chunk_frames;
    }
    let chunk = buffer_data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = frame_bytes as i32;
    *chunk.size_mut() = (total_frames * frame_bytes) as u32;
}

fn read_sample(bytes: &[u8], frame: usize, channel: usize, frame_stride: usize) -> f32 {
    let offset = frame * frame_stride + channel * SAMPLE_BYTES;
    f32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn write_sample(bytes: &mut [u8], frame: usize, channel: usize, frame_stride: usize, sample: f32) {
    let offset = frame * frame_stride + channel * SAMPLE_BYTES;
    bytes[offset..offset + SAMPLE_BYTES].copy_from_slice(&sample.to_le_bytes());
}

fn stream_state_code(state: pw::stream::StreamState) -> u32 {
    match state {
        pw::stream::StreamState::Error(_) => STREAM_FAILED,
        pw::stream::StreamState::Unconnected => STREAM_DISCONNECTED,
        pw::stream::StreamState::Connecting | pw::stream::StreamState::Paused => STREAM_CONNECTING,
        pw::stream::StreamState::Streaming => STREAM_CONNECTED,
    }
}

/// A paused stream carries no audio; ask PipeWire to activate it. This fires
/// on every Paused transition: devices suspend mid-session when idle, and a
/// one-shot guard would leave the stream dead until rebuilt.
fn wake_paused_stream(stream: &pw::stream::Stream, state: &pw::stream::StreamState) {
    if matches!(state, pw::stream::StreamState::Paused) {
        let _ = stream.set_active(true);
    }
}

fn stream_channel_count(endpoint: &AudioEndpoint) -> Result<usize, AudioError> {
    usize::try_from(endpoint.channel_count)
        .ok()
        .filter(|channels| *channels != 0)
        .ok_or_else(|| {
            stream_error(
                "The selected device exposes no audio channels.",
                format!("endpoint {} has zero channels", endpoint.id),
            )
        })
}

fn capture_properties(endpoint: &AudioEndpoint, hub: &ControlHub) -> pw::properties::PropertiesBox {
    let mut props = stream_properties("Capture", hub);
    props.insert("node.name", format!("orion.capture.{}", endpoint.id));
    if matches!(endpoint.endpoint_type, EndpointType::VirtualInput) {
        props.insert("stream.capture.sink", "true");
    }
    props
}

fn playback_properties(
    endpoint: &AudioEndpoint,
    hub: &ControlHub,
) -> pw::properties::PropertiesBox {
    let mut props = stream_properties("Playback", hub);
    props.insert("node.name", format!("orion.playback.{}", endpoint.id));
    props
}

fn stream_properties(category: &str, hub: &ControlHub) -> pw::properties::PropertiesBox {
    let mut props = pw::properties::PropertiesBox::new();
    props.insert("media.type", "Audio");
    props.insert("media.category", category);
    props.insert("media.role", "Production");
    props.insert("application.id", "io.github.gardun0.orion");
    props.insert("application.name", "Orion");
    // Latency hint from Settings, expressed against the stream's own rate so
    // it stays correct at any sample rate. Very small values risk xruns on
    // USB batch devices.
    let hint = hub.buffer_frames();
    if hint > 0 {
        props.insert("node.latency", latency_hint_value(hint, hub.stream_rate()));
    }
    props
}

fn connect_stream(
    stream: &pw::stream::Stream,
    direction: pw::spa::utils::Direction,
    target_id: u32,
    channels: usize,
    sample_rate: u32,
    stream_kind: &str,
) -> Result<(), AudioError> {
    let values = raw_audio_format(channels, sample_rate)?;
    let pod = Pod::from_bytes(&values).ok_or_else(|| {
        stream_error(
            "Orion could not configure the format for that connection.",
            format!("failed to parse {stream_kind} format pod"),
        )
    })?;
    let mut params = [pod];
    stream
        .connect(
            direction,
            Some(target_id),
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|error| {
            stream_error(
                "Orion could not make that connection.",
                format!("failed to connect {stream_kind} stream: {error}"),
            )
        })?;
    // Proactively mark the stream active: PipeWire suspends idle nodes, and
    // a stream that never asks for activation may never be scheduled.
    let _ = stream.set_active(true);
    Ok(())
}

fn raw_audio_format(channels: usize, sample_rate: u32) -> Result<Vec<u8>, AudioError> {
    let channels = u32::try_from(channels).map_err(|_| {
        stream_error(
            "The selected device exposes too many audio channels.",
            format!("channel count {channels} does not fit in PipeWire format"),
        )
    })?;
    let mut audio_info = pw::spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(pw::spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(sample_rate.max(1));
    audio_info.set_channels(channels);
    let object = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(object),
    )
    .map(|serialized| serialized.0.into_inner())
    .map_err(|error| {
        stream_error(
            "Orion could not configure the format for that connection.",
            format!("failed to serialize PipeWire audio format: {error}"),
        )
    })
}

fn dsp_error(error: orion_dsp::DspError) -> AudioError {
    stream_error(
        "Orion could not start realtime processing for that connection.",
        format!("failed to initialize route DSP: {error}"),
    )
}

fn stream_error(user: impl Into<String>, technical: impl Into<String>) -> AudioError {
    AudioError::new(
        ErrorCode::InvalidRoute,
        ErrorSeverity::Error,
        true,
        user,
        technical,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_sample_honors_padded_frame_stride() {
        // Two mono frames padded to 8-byte stride: [sample, 0] each.
        let padded = [
            0.5_f32.to_le_bytes(),
            0.0_f32.to_le_bytes(),
            (-0.5_f32).to_le_bytes(),
            0.0_f32.to_le_bytes(),
        ]
        .concat();
        assert_eq!(read_sample(&padded, 0, 0, 8), 0.5);
        assert_eq!(read_sample(&padded, 1, 0, 8), -0.5);
    }

    #[test]
    fn write_sample_round_trips_through_read_sample() {
        let mut bytes = vec![0_u8; 4 * SAMPLE_BYTES];
        write_sample(&mut bytes, 0, 0, 2 * SAMPLE_BYTES, 0.25);
        write_sample(&mut bytes, 0, 1, 2 * SAMPLE_BYTES, -0.75);
        write_sample(&mut bytes, 1, 0, 2 * SAMPLE_BYTES, 1.5);
        assert_eq!(read_sample(&bytes, 0, 0, 2 * SAMPLE_BYTES), 0.25);
        assert_eq!(read_sample(&bytes, 0, 1, 2 * SAMPLE_BYTES), -0.75);
        assert_eq!(read_sample(&bytes, 1, 0, 2 * SAMPLE_BYTES), 1.5);
    }

    #[test]
    fn latency_hint_uses_stream_rate() {
        assert_eq!(latency_hint_value(512, 48_000), "512/48000");
        assert_eq!(latency_hint_value(512, 768_000), "512/768000");
    }
}

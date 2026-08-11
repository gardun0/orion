//! Realtime-safe control plane shared between the backend command loop and
//! the audio callbacks. Every value the callbacks read lives behind an
//! atomic or an `ArcSwap`; the maps are only touched by the backend thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;

use crate::domain::{AudioEndpoint, EndpointId, GainDb, NormalizedBalance};
use crate::realtime::stage::{db_to_linear, ms_to_delay_frames};

/// Default stream sample rate until the platform reports its graph clock.
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;
/// Default latency hint for newly created streams.
pub const DEFAULT_BUFFER_FRAMES: u32 = 512;
/// Maximum per-endpoint sync delay (`state::MAX_DELAY_MS` mirrors this).
pub const MAX_DELAY_MS: f32 = 500.0;

/// Per-endpoint strip controls (gain/mute/balance), shared with the capture
/// or playback callback of that endpoint's stream. Lock-free on the
/// realtime side; entries are only created by the backend command loop.
pub struct EndpointControls {
    gain_linear: AtomicU32,
    muted: AtomicBool,
    balance: AtomicU32,
}

impl EndpointControls {
    pub fn from_endpoint(endpoint: &AudioEndpoint) -> Self {
        Self {
            gain_linear: AtomicU32::new(db_to_linear(endpoint.gain).to_bits()),
            muted: AtomicBool::new(endpoint.muted),
            balance: AtomicU32::new(endpoint.balance.value().to_bits()),
        }
    }
}

impl Default for EndpointControls {
    fn default() -> Self {
        Self {
            gain_linear: AtomicU32::new(1.0f32.to_bits()),
            muted: AtomicBool::new(false),
            balance: AtomicU32::new(0.0f32.to_bits()),
        }
    }
}

impl EndpointControls {
    pub fn set_gain(&self, gain: GainDb) {
        self.gain_linear
            .store(db_to_linear(gain).to_bits(), Ordering::Relaxed);
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn set_balance(&self, balance: NormalizedBalance) {
        self.balance
            .store(balance.value().to_bits(), Ordering::Relaxed);
    }

    /// Instantaneous gain for metering: meters must track the control, not
    /// wait out the audio ramp.
    pub fn gain_linear(&self) -> f32 {
        if self.muted.load(Ordering::Relaxed) {
            0.0
        } else {
            f32::from_bits(self.gain_linear.load(Ordering::Relaxed))
        }
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub fn gain_target(&self) -> f32 {
        f32::from_bits(self.gain_linear.load(Ordering::Relaxed))
    }

    pub fn balance(&self) -> f32 {
        f32::from_bits(self.balance.load(Ordering::Relaxed))
    }

    pub fn balance_gains(&self) -> (f32, f32) {
        orion_dsp::linear_balance_gains(self.balance())
    }
}

/// Per-endpoint channel processing (sync delay, EQ, channel mapping mode),
/// shared with the endpoint's stream callbacks. Fully atomic/lock-free on
/// the realtime side.
pub struct ChannelControls {
    /// Configured sync delay in ms (f32 bits), re-converted on rate changes.
    delay_ms_bits: AtomicU32,
    /// Sync delay in frames at the current stream rate.
    pub delay_frames: AtomicU32,
    /// Current EQ coefficients, swapped whole so callbacks never tear.
    pub eq: ArcSwap<orion_dsp::EqCoefficients>,
    /// Configured EQ gains in dB (f32 bits: low, mid, high), re-converted to
    /// coefficients on rate changes.
    eq_db_bits: [AtomicU32; 3],
    /// Channel mapping mode (`ChannelMode` code), read per buffer.
    pub mode: AtomicU32,
}

impl Default for ChannelControls {
    fn default() -> Self {
        Self {
            delay_ms_bits: AtomicU32::new(0.0f32.to_bits()),
            delay_frames: AtomicU32::new(0),
            eq: ArcSwap::new(Arc::new(orion_dsp::EqCoefficients::default())),
            eq_db_bits: [
                AtomicU32::new(0.0f32.to_bits()),
                AtomicU32::new(0.0f32.to_bits()),
                AtomicU32::new(0.0f32.to_bits()),
            ],
            mode: AtomicU32::new(crate::domain::ChannelMode::Auto.code()),
        }
    }
}

/// Live engine tuning shared between the backend command loop and every
/// stream's callbacks. Atomics keep it realtime-safe.
pub struct ControlHub {
    /// Sample rate requested by newly created streams (Orion-local; the
    /// platform converts at device boundaries when hardware differs).
    stream_rate: AtomicU32,
    /// Latency hint for newly created streams (`node.latency` on PipeWire).
    buffer_frames: AtomicU32,
    /// Per-endpoint strip controls (gain/mute/balance).
    endpoints: Mutex<HashMap<EndpointId, Arc<EndpointControls>>>,
    /// Per-endpoint channel processing (delay, EQ, mode).
    channels: Mutex<HashMap<EndpointId, Arc<ChannelControls>>>,
}

impl Default for ControlHub {
    fn default() -> Self {
        Self {
            stream_rate: AtomicU32::new(DEFAULT_SAMPLE_RATE),
            buffer_frames: AtomicU32::new(DEFAULT_BUFFER_FRAMES),
            endpoints: Mutex::new(HashMap::new()),
            channels: Mutex::new(HashMap::new()),
        }
    }
}

impl ControlHub {
    pub fn stream_rate(&self) -> u32 {
        self.stream_rate.load(Ordering::Relaxed)
    }

    /// Store a new stream rate, re-converting delays and EQ coefficients.
    /// Returns whether the value actually changed.
    pub fn set_stream_rate(&self, rate: u32) -> bool {
        if self.stream_rate.swap(rate, Ordering::Relaxed) == rate {
            return false;
        }
        let channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        for controls in channels.values() {
            let delay_ms = f32::from_bits(controls.delay_ms_bits.load(Ordering::Relaxed));
            controls
                .delay_frames
                .store(ms_to_delay_frames(delay_ms, rate), Ordering::Relaxed);
            let db = controls
                .eq_db_bits
                .each_ref()
                .map(|b| f32::from_bits(b.load(Ordering::Relaxed)));
            controls.eq.store(Arc::new(orion_dsp::EqCoefficients::new(
                rate, db[0], db[1], db[2],
            )));
        }
        true
    }

    pub fn buffer_frames(&self) -> u32 {
        self.buffer_frames.load(Ordering::Relaxed)
    }

    /// Store a new buffer hint. Returns whether the value actually changed.
    pub fn set_buffer_frames(&self, frames: u32) -> bool {
        self.buffer_frames.swap(frames, Ordering::Relaxed) != frames
    }

    /// Shared strip controls for an endpoint, creating a default entry.
    pub fn endpoint(&self, endpoint: EndpointId) -> Arc<EndpointControls> {
        let mut endpoints = self.endpoints.lock().unwrap_or_else(|e| e.into_inner());
        endpoints.entry(endpoint).or_default().clone()
    }

    /// Shared strip controls seeded from the endpoint's current state when
    /// no entry exists yet (first stream for that endpoint).
    pub fn endpoint_seeded(&self, endpoint: &AudioEndpoint) -> Arc<EndpointControls> {
        let mut endpoints = self.endpoints.lock().unwrap_or_else(|e| e.into_inner());
        endpoints
            .entry(endpoint.id)
            .or_insert_with(|| Arc::new(EndpointControls::from_endpoint(endpoint)))
            .clone()
    }

    /// Shared channel controls for an endpoint, creating a default entry.
    pub fn channel(&self, endpoint: EndpointId) -> Arc<ChannelControls> {
        let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        channels.entry(endpoint).or_default().clone()
    }

    /// Set an endpoint's sync delay in milliseconds; the frame cell updates
    /// atomically so running callbacks pick it up glitch-free.
    pub fn set_delay_ms(&self, endpoint: EndpointId, delay_ms: f32) {
        let delay_ms = delay_ms.clamp(0.0, MAX_DELAY_MS);
        let frames = ms_to_delay_frames(delay_ms, self.stream_rate());
        let controls = self.channel(endpoint);
        controls
            .delay_ms_bits
            .store(delay_ms.to_bits(), Ordering::Relaxed);
        controls.delay_frames.store(frames, Ordering::Relaxed);
    }

    /// Set an endpoint's channel mapping mode; callbacks read it per buffer.
    pub fn set_channel_mode(&self, endpoint: EndpointId, mode: crate::domain::ChannelMode) {
        self.channel(endpoint)
            .mode
            .store(mode.code(), Ordering::Relaxed);
    }

    /// Set an endpoint's 3-band EQ in dB; coefficients are computed here
    /// (off the realtime path) and swapped in whole.
    pub fn set_eq_db(&self, endpoint: EndpointId, low_db: f32, mid_db: f32, high_db: f32) {
        let rate = self.stream_rate();
        let controls = self.channel(endpoint);
        let db = [low_db, mid_db, high_db];
        for (bits, value) in controls.eq_db_bits.iter().zip(db) {
            bits.store(value.to_bits(), Ordering::Relaxed);
        }
        controls.eq.store(Arc::new(orion_dsp::EqCoefficients::new(
            rate, db[0], db[1], db[2],
        )));
    }
}

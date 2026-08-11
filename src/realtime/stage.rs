//! Per-block processing primitives over interleaved `f32` frames. Everything
//! here is allocation-free and callable from a realtime audio callback.

use std::sync::Arc;

use crate::domain::{ChannelMode, GainDb};
use crate::realtime::controls::MAX_DELAY_MS;

/// Longest block the engine processes in one pass; adapters chunk larger
/// native callbacks to this size.
pub const MAX_BLOCK_FRAMES: usize = 4_096;
/// Gain/mute/balance ramps last this long regardless of stream rate, so
/// fader moves stay click-free at 44.1 kHz and at 768 kHz alike.
pub const CONTROL_RAMP_MS: usize = 10;
/// Corrector occupancy target, in quanta: one quantum of buffering is the
/// latency goal; two is the validated upper bound for unstable clocks.
pub const TARGET_QUANTA: u32 = 1;
pub const MAX_TARGET_QUANTA: u32 = 2;
/// Minimum route ring capacity in frames (same-cycle handoff headroom).
const MIN_RING_FRAMES: usize = 16_384;

pub fn sanitize(sample: f32) -> f32 {
    if sample.is_finite() {
        sample
    } else {
        0.0
    }
}

pub fn db_to_linear(gain: GainDb) -> f32 {
    10.0_f32.powf(gain.value() / 20.0)
}

/// Convert a delay in milliseconds to frames at the given stream rate.
pub fn ms_to_delay_frames(delay_ms: f32, rate: u32) -> u32 {
    (delay_ms.max(0.0) / 1_000.0 * rate as f32) as u32
}

/// Delay line capacity in interleaved samples: full delay range at the
/// stream's rate and channel count.
pub fn delay_line_capacity(rate: u32, channels: usize) -> usize {
    let frames = ms_to_delay_frames(MAX_DELAY_MS, rate.max(1)) as usize;
    frames.saturating_mul(channels.max(1)).max(1)
}

/// Control-ramp length in frames: `CONTROL_RAMP_MS` at the given rate.
pub fn control_ramp_frames(rate: u32) -> usize {
    (rate as usize * CONTROL_RAMP_MS / 1_000).max(1)
}

/// Route ring capacity in frames: enough for the corrector target plus
/// scheduling jitter on both clocks, with a floor matching legacy behavior.
pub fn ring_capacity_frames(quantum_frames: u32) -> usize {
    MIN_RING_FRAMES.max(quantum_frames as usize * 4)
}

/// Corrector occupancy target in frames for a stream quantum, clamped to the
/// validated range of one to two quanta of buffering.
pub fn corrector_target_frames(quantum_frames: u32, target_quanta: u32) -> usize {
    let quanta = target_quanta.clamp(1, MAX_TARGET_QUANTA);
    (quantum_frames as usize * quanta as usize).max(1)
}

/// Per-channel gain from a stereo (left, right) pair: channels past the
/// first two pass through at unity, matching the balance law's scope.
pub fn channel_gain(channel: usize, left: f32, right: f32) -> f32 {
    match channel {
        0 => left,
        1 => right,
        _ => 1.0,
    }
}

/// Source sample for one destination channel of a frame. `input` is one
/// interleaved source frame (`source_channels` wide).
///
/// Auto/Stereo keep the endpoint layout mapping: mono sources feed every
/// destination channel, mono destinations average the first two source
/// channels, and otherwise channels map by index (zero-filled when the
/// source is narrower). The remaining modes reshape the frame.
pub fn capture_mode_frame(
    mode: ChannelMode,
    input: &[f32],
    destination_channel: usize,
    destination_channels: usize,
) -> f32 {
    let source_channels = input.len();
    match mode {
        ChannelMode::Mono if source_channels > 1 => (input[0] + input[1]) * 0.5,
        ChannelMode::Left => input[0],
        ChannelMode::Right if source_channels > 1 => input[1],
        ChannelMode::Swap if source_channels > 1 && destination_channels > 1 => {
            input[1 - destination_channel.min(1)]
        }
        _ => mapped_frame(input, destination_channel, destination_channels),
    }
}

fn mapped_frame(input: &[f32], destination_channel: usize, destination_channels: usize) -> f32 {
    if input.len() == 1 {
        input[0]
    } else if destination_channels == 1 {
        (input[0] + input[1]) * 0.5
    } else {
        input.get(destination_channel).copied().unwrap_or(0.0)
    }
}

/// The stereo pair transform behind every channel mode. Auto/Stereo are the
/// identity; used both by the plain post-pass and by the crossfade blend.
pub fn mode_map(mode: ChannelMode, left: f32, right: f32) -> (f32, f32) {
    match mode {
        ChannelMode::Mono => {
            let mono = (left + right) * 0.5;
            (mono, mono)
        }
        ChannelMode::Left => (left, left),
        ChannelMode::Right => (right, right),
        ChannelMode::Swap => (right, left),
        ChannelMode::Auto | ChannelMode::Stereo => (left, right),
    }
}

/// Output-side channel mode as a post-pass over an interleaved block.
/// Stereo-only modes are no-ops for non-stereo streams.
pub fn apply_output_mode(mode: ChannelMode, samples: &mut [f32], channels: usize) {
    if channels != 2 || matches!(mode, ChannelMode::Auto | ChannelMode::Stereo) {
        return;
    }
    for frame in samples.chunks_exact_mut(2) {
        let (left, right) = (frame[0], frame[1]);
        let (new_left, new_right) = mode_map(mode, left, right);
        frame[0] = new_left;
        frame[1] = new_right;
    }
}

/// Blend between two channel modes over `mix` (0.0 = previous, 1.0 = next).
/// Applied to an already-rendered interleaved block; stereo buses only.
pub fn apply_output_mode_crossfade(
    previous: ChannelMode,
    next: ChannelMode,
    mix: &mut orion_dsp::ParameterSmoother,
    samples: &mut [f32],
) {
    for frame in samples.chunks_exact_mut(2) {
        let t = mix.next_value();
        let (old_left, old_right) = mode_map(previous, frame[0], frame[1]);
        let (new_left, new_right) = mode_map(next, frame[0], frame[1]);
        frame[0] = old_left + (new_left - old_left) * t;
        frame[1] = old_right + (new_right - old_right) * t;
    }
}

/// Apply the sync offset through a circular delay line over an interleaved
/// stream. Zero offset is a passthrough.
pub fn apply_sync_offset(
    sample: f32,
    delay_line: &mut [f32],
    write: &mut usize,
    offset_samples: usize,
) -> f32 {
    if offset_samples == 0 {
        return sample;
    }
    let len = delay_line.len();
    delay_line[*write] = sample;
    let read = (*write + len - offset_samples) % len;
    let delayed = delay_line[read];
    *write = (*write + 1) % len;
    delayed
}

/// Pick up a new EQ revision: coefficients are precomputed off-thread and
/// swapped atomically; retune keeps the filter state to avoid transients.
pub fn refresh_eq(
    eqs: &mut [orion_dsp::ChannelEq],
    current: &mut Arc<orion_dsp::EqCoefficients>,
    controls: &crate::realtime::controls::ChannelControls,
) {
    let coefficients = controls.eq.load();
    if !Arc::ptr_eq(&coefficients, current) {
        for eq in eqs.iter_mut() {
            eq.retune(&coefficients);
        }
        *current = Arc::clone(&coefficients);
    }
}

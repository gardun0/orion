//! Lock-free per-route meter shared between an audio callback and the
//! backend's meter publisher. Peak is tracked per sample; RMS and clip are
//! accumulated per block by the single writer (the engine callback) and
//! merged with one atomic update per channel per block.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::realtime::stage::sanitize;

/// One channel's reading for a meter window: absolute peak, RMS over the
/// window's samples, and whether any sample reached full scale.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChannelReading {
    pub peak: f32,
    pub rms: f32,
    pub clipped: bool,
}

pub struct RouteMeter {
    peaks: Box<[AtomicU32]>,
    /// Last emitted level per channel; decays quickly (x0.5 per publish
    /// window) when no new samples arrive, so idle routes fall in ~350 ms
    /// instead of snapping to silence or lingering.
    last: Box<[AtomicU32]>,
    dirty: AtomicBool,
    sequence: AtomicU64,
    /// Window accumulators for RMS: sum of squares (f64 bits) and sample
    /// count. Single writer, swapped out whole by the reader.
    sum_squares: Box<[AtomicU64]>,
    sample_counts: Box<[AtomicU64]>,
    /// Set when any sample in the window reached |x| >= 1.0. For buses this
    /// is measured before the saturator, so clipping stays visible even
    /// though the output itself is bounded.
    clipped: Box<[AtomicBool]>,
    /// RMS decay state for idle windows (mirrors the peak decay).
    last_rms: Box<[AtomicU32]>,
}

impl RouteMeter {
    pub fn new(channels: usize) -> Self {
        Self {
            peaks: (0..channels)
                .map(|_| AtomicU32::new(0.0_f32.to_bits()))
                .collect(),
            last: (0..channels)
                .map(|_| AtomicU32::new(0.0_f32.to_bits()))
                .collect(),
            dirty: AtomicBool::new(false),
            sequence: AtomicU64::new(0),
            sum_squares: (0..channels).map(|_| AtomicU64::new(0)).collect(),
            sample_counts: (0..channels).map(|_| AtomicU64::new(0)).collect(),
            clipped: (0..channels).map(|_| AtomicBool::new(false)).collect(),
            last_rms: (0..channels)
                .map(|_| AtomicU32::new(0.0_f32.to_bits()))
                .collect(),
        }
    }

    pub fn observe(&self, channel: usize, sample: f32) {
        let value = sanitize(sample).abs().min(1.0);
        self.dirty.store(true, Ordering::Relaxed);
        let peak = &self.peaks[channel];
        let mut current = peak.load(Ordering::Relaxed);
        while value > f32::from_bits(current) {
            match peak.compare_exchange_weak(
                current,
                value.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    /// Merge one processed block into the window. Called by the engine
    /// callback — the meter's only writer — so the accumulation is a plain
    /// read-modify-write, never a cross-thread race worth a CAS loop storm.
    pub fn merge_block_stats(
        &self,
        channel: usize,
        sum_squares: f64,
        sample_count: u64,
        clipped: bool,
    ) {
        self.dirty.store(true, Ordering::Relaxed);
        let cell = &self.sum_squares[channel];
        let _ = cell.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bits| {
            Some((f64::from_bits(bits) + sum_squares).to_bits())
        });
        self.sample_counts[channel].fetch_add(sample_count, Ordering::Relaxed);
        if clipped {
            self.clipped[channel].store(true, Ordering::Relaxed);
        }
    }

    /// Current reading per channel: fresh peaks/RMS when audio flowed this
    /// window, otherwise the previous values decayed by 30%.
    pub fn readings(&self) -> impl Iterator<Item = ChannelReading> + '_ {
        let fresh = self.dirty.swap(false, Ordering::Relaxed);
        (0..self.peaks.len()).map(move |channel| {
            let peak = &self.peaks[channel];
            let last = &self.last[channel];
            let last_rms = &self.last_rms[channel];
            let clipped = self.clipped[channel].swap(false, Ordering::Relaxed);
            let (peak, rms) = if fresh {
                let peak = f32::from_bits(peak.swap(0.0_f32.to_bits(), Ordering::Relaxed));
                last.store(peak.to_bits(), Ordering::Relaxed);
                let sum_squares =
                    f64::from_bits(self.sum_squares[channel].swap(0, Ordering::Relaxed));
                let count = self.sample_counts[channel].swap(0, Ordering::Relaxed);
                let rms = if count > 0 {
                    (sum_squares / count as f64).sqrt() as f32
                } else {
                    0.0
                };
                last_rms.store(rms.to_bits(), Ordering::Relaxed);
                (peak, rms)
            } else {
                // Idle fall: 0.5 per publish window (~350 ms to the floor).
                let peak = f32::from_bits(last.load(Ordering::Relaxed)) * 0.5;
                let rms = f32::from_bits(last_rms.load(Ordering::Relaxed)) * 0.5;
                last.store(peak.to_bits(), Ordering::Relaxed);
                last_rms.store(rms.to_bits(), Ordering::Relaxed);
                (peak, rms)
            };
            ChannelReading {
                peak,
                rms,
                clipped: clipped && fresh,
            }
        })
    }

    pub fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }
}

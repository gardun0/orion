//! Lock-free per-route peak meter shared between an audio callback and the
//! backend's meter publisher.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::realtime::stage::sanitize;

pub struct RouteMeter {
    peaks: Box<[AtomicU32]>,
    /// Last emitted level per channel; decays when no new samples arrive so
    /// idle routes fall smoothly instead of snapping to silence.
    last: Box<[AtomicU32]>,
    dirty: AtomicBool,
    sequence: AtomicU64,
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
        }
    }

    pub fn channels(&self) -> usize {
        self.peaks.len()
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

    /// Current level per channel: fresh peaks when audio flowed this window,
    /// otherwise the previous level decayed by 30%.
    pub fn levels(&self) -> impl Iterator<Item = f32> + '_ {
        let fresh = self.dirty.swap(false, Ordering::Relaxed);
        (0..self.peaks.len()).map(move |channel| {
            let peak = &self.peaks[channel];
            let last = &self.last[channel];
            if fresh {
                let value = f32::from_bits(peak.swap(0.0_f32.to_bits(), Ordering::Relaxed));
                last.store(value.to_bits(), Ordering::Relaxed);
                value
            } else {
                let previous = f32::from_bits(last.load(Ordering::Relaxed));
                let decayed = previous * 0.7;
                last.store(decayed.to_bits(), Ordering::Relaxed);
                decayed
            }
        })
    }

    pub fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }
}

use crate::buffer::MAX_CHANNELS;
use crate::DspError;

/// PI controller tuning for drift tracking: ring occupancy error in frames is
/// mapped to a consumption-rate adjustment in parts per million.
const KP_PER_FRAME: f64 = 1.0e-6;
const KI_PER_FRAME: f64 = 2.0e-8;
const INTEGRAL_LIMIT: f64 = 512.0;
/// Never correct more than ±2000 ppm (0.2%) — real device drift is far below.
const MAX_ADJUST: f64 = 0.002;
/// Slew limit per callback so ratio changes never wobble the pitch.
const MAX_RATIO_STEP: f64 = 2.0e-5;
/// An underrun is held this long (25 ms) — enough for real scheduling jitter,
/// short enough that a paused source reads as silence on the meters.
const HOLD_LIMIT_MS: u32 = 25;

/// Adaptive drift corrector for a producer/consumer ring between two devices
/// with independent clocks. The consumer reads through a fractional pointer
/// and linearly interpolates between frames; a PI controller nudges the
/// consumption rate so occupancy stays near the target. Unlike chunk-based
/// resamplers it consumes exactly `floor(accumulated ratio)` frames, so it
/// works with any callback size and any device without oversubscription.
///
/// At the nominal ratio (1.0) the output is the input delayed by exactly one
/// frame — the interpolator needs two points — with no other coloration.
///
/// Underrun behavior: the last real frame is held while the ring is starved,
/// but only for `HOLD_LIMIT_MS` — past that the interpolator glides to
/// silence, so a paused source reads as silence instead of holding a level
/// (and a constant DC tone) forever.
pub struct DriftCorrector {
    target_frames: usize,
    integral: f64,
    ratio: f64,
    frac: f64,
    // One held frame per channel; sized once at construction, never grows.
    previous: Vec<f32>,
    current: Vec<f32>,
    /// Consecutive starved frames so far, and the bound (25 ms) after which
    /// the held frame is released to silence.
    starved_frames: usize,
    hold_limit_frames: usize,
}

impl DriftCorrector {
    pub fn new(channels: usize, target_frames: usize, sample_rate: u32) -> Result<Self, DspError> {
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(DspError::InvalidChannelCount(channels));
        }
        if sample_rate == 0 {
            return Err(DspError::InvalidSampleRate(0));
        }
        Ok(Self {
            target_frames,
            integral: 0.0,
            ratio: 1.0,
            frac: 0.0,
            previous: vec![0.0; channels],
            current: vec![0.0; channels],
            starved_frames: 0,
            hold_limit_frames: (sample_rate as usize / 1_000 * HOLD_LIMIT_MS as usize).max(1),
        })
    }

    pub const fn ratio(&self) -> f64 {
        self.ratio
    }

    pub fn set_target_frames(&mut self, target_frames: usize) {
        self.target_frames = target_frames;
    }

    /// Write `output` frames of `channels` interleaved samples, pulling from
    /// `pull` at the drift-adjusted rate. `pull` returns None on underrun; the
    /// corrector holds the last real frame for up to 25 ms, then glides to
    /// silence (one interpolation step — no click).
    pub fn process(
        &mut self,
        output: &mut [f32],
        channels: usize,
        buffered_frames: usize,
        pull: &mut dyn FnMut() -> Option<f32>,
    ) {
        self.update_ratio(buffered_frames);
        let frames = output.len() / channels;
        for frame in 0..frames {
            self.frac += self.ratio;
            while self.frac >= 1.0 {
                // clone_from reuses the existing allocation (no RT alloc).
                self.previous.clone_from(&self.current);
                let mut got_sample = false;
                for channel in 0..channels {
                    if let Some(sample) = pull() {
                        self.current[channel] = sample;
                        got_sample = true;
                    }
                }
                if got_sample {
                    self.starved_frames = 0;
                } else {
                    self.starved_frames += 1;
                    if self.starved_frames >= self.hold_limit_frames {
                        // Starved past the bound: release to silence; the
                        // previous->current interpolation glides there in one
                        // step, so no click.
                        self.current.fill(0.0);
                    }
                }
                self.frac -= 1.0;
            }
            let frac = self.frac as f32;
            for channel in 0..channels {
                output[frame * channels + channel] = self.previous[channel]
                    + (self.current[channel] - self.previous[channel]) * frac;
            }
        }
    }

    /// Ring above target → consume faster (ratio > 1); below → hold back.
    /// Corrections are clamped, integral-limited and slew-limited per callback.
    fn update_ratio(&mut self, buffered_frames: usize) {
        let error = buffered_frames as f64 - self.target_frames as f64;
        self.integral = (self.integral + error).clamp(-INTEGRAL_LIMIT, INTEGRAL_LIMIT);
        let adjust =
            (KP_PER_FRAME * error + KI_PER_FRAME * self.integral).clamp(-MAX_ADJUST, MAX_ADJUST);
        let desired = 1.0 + adjust;
        let step = (desired - self.ratio).clamp(-MAX_RATIO_STEP, MAX_RATIO_STEP);
        self.ratio += step;
    }
}

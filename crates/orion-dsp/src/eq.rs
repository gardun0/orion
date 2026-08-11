//! Three-band channel EQ: biquad shelving/bell filters tuned per strip.
//!
//! Coefficients are computed outside the realtime callback (see
//! `EqCoefficients`) and copied in per buffer; the biquad state itself is
//! plain Direct Form II Transposed, allocation-free and branch-free.

/// Minimum/maximum band gain in decibels (knob range).
pub const EQ_MIN_DB: f32 = -12.0;
pub const EQ_MAX_DB: f32 = 12.0;

/// Low shelf corner frequency.
pub const EQ_LOW_HZ: f32 = 250.0;
/// Mid peaking center frequency.
pub const EQ_MID_HZ: f32 = 1_000.0;
/// High shelf corner frequency.
pub const EQ_HIGH_HZ: f32 = 8_000.0;
/// Peaking band Q.
const MID_Q: f32 = 0.9;

/// A single biquad filter, Direct Form II Transposed. The default is the
/// identity (b0 = 1): an unconfigured filter must pass audio, not mute it.
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }
}

impl Biquad {
    /// Set coefficients (already normalized by a0) and clear the state.
    pub fn set_coefficients(&mut self, b0: f32, b1: f32, b2: f32, a1: f32, a2: f32) {
        self.b0 = b0;
        self.b1 = b1;
        self.b2 = b2;
        self.a1 = a1;
        self.a2 = a2;
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    /// Swap coefficients while keeping the filter state: live knob changes
    /// avoid the larger transient a state reset would cause.
    pub fn retune(&mut self, b0: f32, b1: f32, b2: f32, a1: f32, a2: f32) {
        self.b0 = b0;
        self.b1 = b1;
        self.b2 = b2;
        self.a1 = a1;
        self.a2 = a2;
    }

    #[inline]
    pub fn process(&mut self, sample: f32) -> f32 {
        let out = sample * self.b0 + self.z1;
        self.z1 = sample * self.b1 + self.z2 - self.a1 * out;
        self.z2 = sample * self.b2 - self.a2 * out;
        out
    }
}

/// Coefficient set for the three bands at a given sample rate, computed from
/// dB gains. All trigonometry happens here, off the realtime path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqCoefficients {
    pub low: [f32; 5],
    pub mid: [f32; 5],
    pub high: [f32; 5],
}

impl Default for EqCoefficients {
    /// 0 dB on every band; the exact rate does not matter for passthrough.
    fn default() -> Self {
        Self::new(48_000, 0.0, 0.0, 0.0)
    }
}

impl EqCoefficients {
    pub fn new(sample_rate: u32, low_db: f32, mid_db: f32, high_db: f32) -> Self {
        let rate = sample_rate.max(1) as f32;
        Self {
            low: shelf_coefficients(EQ_LOW_HZ, low_db, rate, true),
            mid: peaking_coefficients(EQ_MID_HZ, mid_db, rate),
            high: shelf_coefficients(EQ_HIGH_HZ, high_db, rate, false),
        }
    }
}

/// RBJ Audio EQ Cookbook shelf. `low` selects the low-shelf variant.
fn shelf_coefficients(freq_hz: f32, gain_db: f32, rate: f32, low: bool) -> [f32; 5] {
    let amplitude = 10f32.powf(gain_db / 40.0);
    let omega = 2.0 * std::f32::consts::PI * freq_hz / rate;
    let (sin, cos) = omega.sin_cos();
    // S=1 shelf slope.
    let alpha = sin / 2.0 * std::f32::consts::SQRT_2;
    let two_sqrt_a_alpha = 2.0 * amplitude.sqrt() * alpha;
    let a_minus = amplitude - 1.0;
    let a_plus = amplitude + 1.0;

    let (b0, b1, b2, a0, a1, a2) = if low {
        let b0 = amplitude * (a_plus - a_minus * cos + two_sqrt_a_alpha);
        let b1 = 2.0 * amplitude * (a_minus - a_plus * cos);
        let b2 = amplitude * (a_plus - a_minus * cos - two_sqrt_a_alpha);
        let a0 = a_plus + a_minus * cos + two_sqrt_a_alpha;
        let a1 = -2.0 * (a_minus + a_plus * cos);
        let a2 = a_plus + a_minus * cos - two_sqrt_a_alpha;
        (b0, b1, b2, a0, a1, a2)
    } else {
        let b0 = amplitude * (a_plus + a_minus * cos + two_sqrt_a_alpha);
        let b1 = -2.0 * amplitude * (a_minus + a_plus * cos);
        let b2 = amplitude * (a_plus + a_minus * cos - two_sqrt_a_alpha);
        let a0 = a_plus - a_minus * cos + two_sqrt_a_alpha;
        let a1 = 2.0 * (a_minus - a_plus * cos);
        let a2 = a_plus - a_minus * cos - two_sqrt_a_alpha;
        (b0, b1, b2, a0, a1, a2)
    };
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// RBJ Audio EQ Cookbook peaking band.
fn peaking_coefficients(freq_hz: f32, gain_db: f32, rate: f32) -> [f32; 5] {
    let amplitude = 10f32.powf(gain_db / 40.0);
    let omega = 2.0 * std::f32::consts::PI * freq_hz / rate;
    let (sin, cos) = omega.sin_cos();
    let alpha = sin / (2.0 * MID_Q);

    let b0 = 1.0 + alpha * amplitude;
    let b1 = -2.0 * cos;
    let b2 = 1.0 - alpha * amplitude;
    let a0 = 1.0 + alpha / amplitude;
    let a1 = -2.0 * cos;
    let a2 = 1.0 - alpha / amplitude;
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// Three-band EQ for one channel: low shelf, mid bell, high shelf in series.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChannelEq {
    low: Biquad,
    mid: Biquad,
    high: Biquad,
}

impl ChannelEq {
    /// Retune the bands; state is cleared to avoid transients from stale
    /// delay-line content after a large coefficient swing.
    pub fn set_coefficients(&mut self, coefficients: &EqCoefficients) {
        self.low.set_coefficients(
            coefficients.low[0],
            coefficients.low[1],
            coefficients.low[2],
            coefficients.low[3],
            coefficients.low[4],
        );
        self.mid.set_coefficients(
            coefficients.mid[0],
            coefficients.mid[1],
            coefficients.mid[2],
            coefficients.mid[3],
            coefficients.mid[4],
        );
        self.high.set_coefficients(
            coefficients.high[0],
            coefficients.high[1],
            coefficients.high[2],
            coefficients.high[3],
            coefficients.high[4],
        );
    }

    /// Live retune: swap coefficients but keep the filter state.
    pub fn retune(&mut self, coefficients: &EqCoefficients) {
        self.low.retune(
            coefficients.low[0],
            coefficients.low[1],
            coefficients.low[2],
            coefficients.low[3],
            coefficients.low[4],
        );
        self.mid.retune(
            coefficients.mid[0],
            coefficients.mid[1],
            coefficients.mid[2],
            coefficients.mid[3],
            coefficients.mid[4],
        );
        self.high.retune(
            coefficients.high[0],
            coefficients.high[1],
            coefficients.high[2],
            coefficients.high[3],
            coefficients.high[4],
        );
    }

    #[inline]
    pub fn process(&mut self, sample: f32) -> f32 {
        self.high
            .process(self.mid.process(self.low.process(sample)))
    }
}

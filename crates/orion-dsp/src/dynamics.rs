//! Always-on output protection: a memoryless soft-knee saturator.
//!
//! The curve is the identity below the knee, so material that never leaves
//! digital headroom passes through untouched; above the knee a tanh bend
//! compresses toward ±1.0. It is C1-continuous (no kink at the knee, hence
//! no click), symmetric, stateless, and safe for non-finite input.

/// Knee position in linear amplitude: -1 dBFS of headroom for the bend.
pub const SOFT_CLIP_KNEE: f32 = 0.891_250_9; // 10^(-1/20)

/// Soft-clip `sample` to the ±1.0 range. Non-finite input maps to silence:
/// a blown-up upstream value must never reach the wire.
pub fn soft_clip(sample: f32) -> f32 {
    if !sample.is_finite() {
        return 0.0;
    }
    let magnitude = sample.abs();
    if magnitude <= SOFT_CLIP_KNEE {
        return sample;
    }
    let headroom = 1.0 - SOFT_CLIP_KNEE;
    let over = (magnitude - SOFT_CLIP_KNEE) / headroom;
    sample.signum() * (SOFT_CLIP_KNEE + headroom * over.tanh())
}

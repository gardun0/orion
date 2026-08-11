use crate::buffer::validate_process;
use crate::{AudioBuffer, DspError, ProcessConfig, ProcessContext};

pub trait AudioProcessor: Send {
    fn prepare(&mut self, config: ProcessConfig) -> Result<(), DspError>;
    fn process(
        &mut self,
        context: &ProcessContext,
        buffer: &mut AudioBuffer<'_, '_>,
    ) -> Result<(), DspError>;
    fn reset(&mut self);
    fn latency_frames(&self) -> usize;
}

#[derive(Debug, Clone, Copy)]
pub struct ParameterSmoother {
    current: f32,
    target: f32,
    step: f32,
    remaining_frames: usize,
}

impl ParameterSmoother {
    pub fn new(value: f32) -> Result<Self, DspError> {
        validate_finite(value, "smoother value")?;
        Ok(Self {
            current: value,
            target: value,
            step: 0.0,
            remaining_frames: 0,
        })
    }

    pub const fn current(&self) -> f32 {
        self.current
    }

    pub const fn target(&self) -> f32 {
        self.target
    }

    pub const fn remaining_frames(&self) -> usize {
        self.remaining_frames
    }

    pub const fn is_smoothing(&self) -> bool {
        self.remaining_frames != 0
    }

    pub fn set_target(&mut self, target: f32, ramp_frames: usize) -> Result<(), DspError> {
        validate_finite(target, "smoother target")?;
        self.target = target;
        if ramp_frames == 0 {
            self.current = target;
            self.step = 0.0;
            self.remaining_frames = 0;
        } else {
            self.step = (target - self.current) / ramp_frames as f32;
            self.remaining_frames = ramp_frames;
        }
        Ok(())
    }

    pub fn next_value(&mut self) -> f32 {
        if self.remaining_frames != 0 {
            self.current += self.step;
            self.remaining_frames -= 1;
            if self.remaining_frames == 0 {
                self.current = self.target;
            }
        }
        self.current
    }

    pub fn reset(&mut self, value: f32) -> Result<(), DspError> {
        validate_finite(value, "smoother reset value")?;
        self.current = value;
        self.target = value;
        self.step = 0.0;
        self.remaining_frames = 0;
        Ok(())
    }

    fn settle_at_target(&mut self) {
        self.current = self.target;
        self.step = 0.0;
        self.remaining_frames = 0;
    }
}

#[derive(Debug)]
pub struct GainProcessor {
    prepared: Option<ProcessConfig>,
    gain: ParameterSmoother,
    audible: ParameterSmoother,
}

impl GainProcessor {
    pub fn new(initial_gain: f32) -> Result<Self, DspError> {
        validate_gain(initial_gain)?;
        Ok(Self {
            prepared: None,
            gain: ParameterSmoother::new(initial_gain)?,
            audible: ParameterSmoother::new(1.0)?,
        })
    }

    pub fn set_target_gain(&mut self, gain: f32, ramp_frames: usize) -> Result<(), DspError> {
        validate_gain(gain)?;
        self.gain.set_target(gain, ramp_frames)
    }

    pub fn set_muted(&mut self, muted: bool, ramp_frames: usize) -> Result<(), DspError> {
        self.audible
            .set_target(if muted { 0.0 } else { 1.0 }, ramp_frames)
    }

    pub const fn target_gain(&self) -> f32 {
        self.gain.target()
    }

    pub fn is_muted(&self) -> bool {
        self.audible.target() == 0.0
    }
}

impl AudioProcessor for GainProcessor {
    fn prepare(&mut self, config: ProcessConfig) -> Result<(), DspError> {
        self.prepared = Some(config);
        Ok(())
    }

    fn process(
        &mut self,
        context: &ProcessContext,
        buffer: &mut AudioBuffer<'_, '_>,
    ) -> Result<(), DspError> {
        validate_process(self.prepared, context, buffer)?;
        for frame in 0..context.frames() {
            let gain = self.gain.next_value() * self.audible.next_value();
            for channel in 0..buffer.channel_count() {
                buffer.channel_mut(channel)[frame] *= gain;
            }
        }
        Ok(())
    }

    fn reset(&mut self) {
        self.gain.settle_at_target();
        self.audible.settle_at_target();
    }

    fn latency_frames(&self) -> usize {
        0
    }
}

#[derive(Debug)]
pub struct StereoBalanceProcessor {
    prepared: Option<ProcessConfig>,
    balance: ParameterSmoother,
}

impl StereoBalanceProcessor {
    pub fn new(initial_balance: f32) -> Result<Self, DspError> {
        validate_pan(initial_balance, "balance")?;
        Ok(Self {
            prepared: None,
            balance: ParameterSmoother::new(initial_balance)?,
        })
    }

    pub fn set_target_balance(&mut self, balance: f32, ramp_frames: usize) -> Result<(), DspError> {
        validate_pan(balance, "balance")?;
        self.balance.set_target(balance, ramp_frames)
    }
}

impl AudioProcessor for StereoBalanceProcessor {
    fn prepare(&mut self, config: ProcessConfig) -> Result<(), DspError> {
        if config.channel_count() != 2 {
            return Err(DspError::ChannelCountMismatch {
                expected: 2,
                actual: config.channel_count(),
            });
        }
        self.prepared = Some(config);
        Ok(())
    }

    fn process(
        &mut self,
        context: &ProcessContext,
        buffer: &mut AudioBuffer<'_, '_>,
    ) -> Result<(), DspError> {
        validate_process(self.prepared, context, buffer)?;
        for frame in 0..context.frames() {
            let (left_gain, right_gain) = linear_balance_gains(self.balance.next_value());
            buffer.channel_mut(0)[frame] *= left_gain;
            buffer.channel_mut(1)[frame] *= right_gain;
        }
        Ok(())
    }

    fn reset(&mut self) {
        self.balance.settle_at_target();
    }

    fn latency_frames(&self) -> usize {
        0
    }
}

/// Linear stereo balance law: attenuate only the side opposite the turn, so
/// center is unity gain on both channels and there is no hidden boost.
/// `balance` is clamped to [-1, 1]: negative favors left, positive right.
pub fn linear_balance_gains(balance: f32) -> (f32, f32) {
    let balance = balance.clamp(-1.0, 1.0);
    (
        if balance > 0.0 { 1.0 - balance } else { 1.0 },
        if balance < 0.0 { 1.0 + balance } else { 1.0 },
    )
}

pub fn constant_power_pan_gains(pan: f32) -> Result<(f32, f32), DspError> {
    validate_pan(pan, "pan")?;
    if pan == -1.0 {
        return Ok((1.0, 0.0));
    }
    if pan == 1.0 {
        return Ok((0.0, 1.0));
    }
    let angle = (pan + 1.0) * core::f32::consts::FRAC_PI_4;
    Ok((angle.cos(), angle.sin()))
}

pub fn pan_mono_to_stereo(
    input: &[f32],
    left: &mut [f32],
    right: &mut [f32],
    pan: f32,
) -> Result<(), DspError> {
    if left.len() != input.len() {
        return Err(DspError::FrameCountMismatch {
            expected: input.len(),
            actual: left.len(),
        });
    }
    if right.len() != input.len() {
        return Err(DspError::FrameCountMismatch {
            expected: input.len(),
            actual: right.len(),
        });
    }
    let (left_gain, right_gain) = constant_power_pan_gains(pan)?;
    for frame in 0..input.len() {
        left[frame] = input[frame] * left_gain;
        right[frame] = input[frame] * right_gain;
    }
    Ok(())
}

fn validate_finite(value: f32, name: &'static str) -> Result<(), DspError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(DspError::InvalidParameter(name))
    }
}

fn validate_gain(gain: f32) -> Result<(), DspError> {
    if gain.is_finite() && gain >= 0.0 {
        Ok(())
    } else {
        Err(DspError::InvalidParameter("gain"))
    }
}

fn validate_pan(pan: f32, name: &'static str) -> Result<(), DspError> {
    if pan.is_finite() && (-1.0..=1.0).contains(&pan) {
        Ok(())
    } else {
        Err(DspError::InvalidParameter(name))
    }
}

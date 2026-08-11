use crate::{AudioBuffer, DspError, ProcessConfig, ProcessContext};

#[derive(Debug)]
pub struct MixRoute<'buffer, 'channels, 'samples> {
    input: &'buffer AudioBuffer<'channels, 'samples>,
    gain: f32,
}

impl<'buffer, 'channels, 'samples> MixRoute<'buffer, 'channels, 'samples> {
    pub fn new(
        input: &'buffer AudioBuffer<'channels, 'samples>,
        gain: f32,
    ) -> Result<Self, DspError> {
        validate_route_gain(gain)?;
        Ok(Self { input, gain })
    }

    pub const fn input(&self) -> &AudioBuffer<'channels, 'samples> {
        self.input
    }

    pub const fn gain(&self) -> f32 {
        self.gain
    }

    pub fn set_gain(&mut self, gain: f32) -> Result<(), DspError> {
        validate_route_gain(gain)?;
        self.gain = gain;
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct Mixer {
    prepared: Option<ProcessConfig>,
}

impl Mixer {
    pub const fn new() -> Self {
        Self { prepared: None }
    }

    pub fn prepare(&mut self, config: ProcessConfig) {
        self.prepared = Some(config);
    }

    pub fn reset(&mut self) {}

    pub const fn latency_frames(&self) -> usize {
        0
    }

    pub fn process(
        &mut self,
        context: &ProcessContext,
        routes: &[MixRoute<'_, '_, '_>],
        output: &mut AudioBuffer<'_, '_>,
    ) -> Result<(), DspError> {
        let config = self.prepared.ok_or(DspError::NotPrepared)?;
        if context.config() != config {
            return Err(DspError::ConfigurationMismatch);
        }
        if output.channel_count() != config.channel_count() {
            return Err(DspError::ChannelCountMismatch {
                expected: config.channel_count(),
                actual: output.channel_count(),
            });
        }
        validate_frames(context, output.frames())?;
        for route in routes {
            validate_frames(context, route.input.frames())?;
            validate_route_gain(route.gain)?;
        }

        output.silence();
        for route in routes {
            mix_route(route.input, output, route.gain);
        }
        Ok(())
    }
}

fn validate_frames(context: &ProcessContext, frames: usize) -> Result<(), DspError> {
    if frames == context.frames() {
        Ok(())
    } else {
        Err(DspError::FrameCountMismatch {
            expected: context.frames(),
            actual: frames,
        })
    }
}

fn validate_route_gain(gain: f32) -> Result<(), DspError> {
    if gain.is_finite() {
        Ok(())
    } else {
        Err(DspError::InvalidParameter("route gain"))
    }
}

fn mix_route(input: &AudioBuffer<'_, '_>, output: &mut AudioBuffer<'_, '_>, gain: f32) {
    match (input.channel_count(), output.channel_count()) {
        (1, 1) => {
            for frame in 0..output.frames() {
                output.channel_mut(0)[frame] += input.channel(0)[frame] * gain;
            }
        }
        (1, 2) => {
            for frame in 0..output.frames() {
                let sample = input.channel(0)[frame] * gain;
                output.channel_mut(0)[frame] += sample;
                output.channel_mut(1)[frame] += sample;
            }
        }
        (2, 2) => {
            for frame in 0..output.frames() {
                output.channel_mut(0)[frame] += input.channel(0)[frame] * gain;
                output.channel_mut(1)[frame] += input.channel(1)[frame] * gain;
            }
        }
        (2, 1) => {
            for frame in 0..output.frames() {
                let mono = (input.channel(0)[frame] + input.channel(1)[frame]) * 0.5;
                output.channel_mut(0)[frame] += mono * gain;
            }
        }
        _ => unreachable!("AudioBuffer validates one or two channels"),
    }
}

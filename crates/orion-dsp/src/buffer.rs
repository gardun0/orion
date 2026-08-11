use crate::DspError;

/// Crate-wide ceiling on channel count. Block processors stay mono/stereo for
/// now; the ceiling exists for stream-level components (e.g. drift correction)
/// that sit on interleaved buses of any realistic width.
pub const MAX_CHANNELS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessConfig {
    sample_rate: u32,
    max_block_size: usize,
    channel_count: usize,
}

impl ProcessConfig {
    pub fn new(
        sample_rate: u32,
        max_block_size: usize,
        channel_count: usize,
    ) -> Result<Self, DspError> {
        if sample_rate == 0 {
            return Err(DspError::InvalidSampleRate(sample_rate));
        }
        if max_block_size == 0 {
            return Err(DspError::InvalidMaxBlockSize(max_block_size));
        }
        if !(1..=2).contains(&channel_count) {
            return Err(DspError::InvalidChannelCount(channel_count));
        }

        Ok(Self {
            sample_rate,
            max_block_size,
            channel_count,
        })
    }

    pub const fn sample_rate(self) -> u32 {
        self.sample_rate
    }

    pub const fn max_block_size(self) -> usize {
        self.max_block_size
    }

    pub const fn channel_count(self) -> usize {
        self.channel_count
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessContext {
    config: ProcessConfig,
    frames: usize,
    frame_position: u64,
}

impl ProcessContext {
    pub fn new(
        config: ProcessConfig,
        frames: usize,
        frame_position: u64,
    ) -> Result<Self, DspError> {
        if frames > config.max_block_size {
            return Err(DspError::BlockTooLarge {
                frames,
                max_frames: config.max_block_size,
            });
        }

        Ok(Self {
            config,
            frames,
            frame_position,
        })
    }

    pub const fn config(self) -> ProcessConfig {
        self.config
    }

    pub const fn frames(self) -> usize {
        self.frames
    }

    pub const fn frame_position(self) -> u64 {
        self.frame_position
    }
}

#[derive(Debug)]
pub struct AudioBuffer<'channels, 'samples> {
    channels: &'channels mut [&'samples mut [f32]],
    frames: usize,
}

impl<'channels, 'samples> AudioBuffer<'channels, 'samples> {
    pub fn new(channels: &'channels mut [&'samples mut [f32]]) -> Result<Self, DspError> {
        if !(1..=2).contains(&channels.len()) {
            return Err(DspError::InvalidChannelCount(channels.len()));
        }

        let frames = channels[0].len();
        for (channel, samples) in channels.iter().enumerate().skip(1) {
            if samples.len() != frames {
                return Err(DspError::NonUniformBuffer {
                    expected_frames: frames,
                    channel,
                    actual_frames: samples.len(),
                });
            }
        }

        Ok(Self { channels, frames })
    }

    pub const fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub const fn frames(&self) -> usize {
        self.frames
    }

    pub fn channel(&self, channel: usize) -> &[f32] {
        self.channels[channel]
    }

    pub fn channel_mut(&mut self, channel: usize) -> &mut [f32] {
        self.channels[channel]
    }

    pub fn silence(&mut self) {
        for channel in self.channels.iter_mut() {
            channel.fill(0.0);
        }
    }
}

pub(crate) fn validate_process(
    prepared: Option<ProcessConfig>,
    context: &ProcessContext,
    buffer: &AudioBuffer<'_, '_>,
) -> Result<ProcessConfig, DspError> {
    let prepared = prepared.ok_or(DspError::NotPrepared)?;
    if context.config != prepared {
        return Err(DspError::ConfigurationMismatch);
    }
    if buffer.channel_count() != prepared.channel_count {
        return Err(DspError::ChannelCountMismatch {
            expected: prepared.channel_count,
            actual: buffer.channel_count(),
        });
    }
    if buffer.frames() != context.frames {
        return Err(DspError::FrameCountMismatch {
            expected: context.frames,
            actual: buffer.frames(),
        });
    }
    Ok(prepared)
}

use core::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum DspError {
    InvalidSampleRate(u32),
    InvalidMaxBlockSize(usize),
    InvalidChannelCount(usize),
    NonUniformBuffer {
        expected_frames: usize,
        channel: usize,
        actual_frames: usize,
    },
    BlockTooLarge {
        frames: usize,
        max_frames: usize,
    },
    FrameCountMismatch {
        expected: usize,
        actual: usize,
    },
    ChannelCountMismatch {
        expected: usize,
        actual: usize,
    },
    ConfigurationMismatch,
    NotPrepared,
    InvalidParameter(&'static str),
}

impl fmt::Display for DspError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSampleRate(rate) => write!(formatter, "invalid sample rate: {rate}"),
            Self::InvalidMaxBlockSize(size) => {
                write!(formatter, "invalid maximum block size: {size}")
            }
            Self::InvalidChannelCount(count) => write!(formatter, "invalid channel count: {count}"),
            Self::NonUniformBuffer {
                expected_frames,
                channel,
                actual_frames,
            } => write!(
                formatter,
                "channel {channel} has {actual_frames} frames, expected {expected_frames}"
            ),
            Self::BlockTooLarge { frames, max_frames } => {
                write!(
                    formatter,
                    "block has {frames} frames, maximum is {max_frames}"
                )
            }
            Self::FrameCountMismatch { expected, actual } => {
                write!(formatter, "buffer has {actual} frames, expected {expected}")
            }
            Self::ChannelCountMismatch { expected, actual } => {
                write!(
                    formatter,
                    "buffer has {actual} channels, expected {expected}"
                )
            }
            Self::ConfigurationMismatch => {
                formatter.write_str("process context does not match prepared configuration")
            }
            Self::NotPrepared => formatter.write_str("processor has not been prepared"),
            Self::InvalidParameter(name) => write!(formatter, "invalid parameter: {name}"),
        }
    }
}

impl std::error::Error for DspError {}

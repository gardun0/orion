mod buffer;
mod drift;
mod eq;
mod error;
mod meter;
mod mixer;
mod processors;

pub use buffer::{AudioBuffer, ProcessConfig, ProcessContext, MAX_CHANNELS};
pub use dasp::{Frame, Sample, Signal};
pub use drift::DriftCorrector;
pub use eq::{Biquad, ChannelEq, EqCoefficients, EQ_MAX_DB, EQ_MIN_DB};
pub use error::DspError;
pub use meter::ChannelMeter;
pub use mixer::{MixRoute, Mixer};
pub use processors::{
    constant_power_pan_gains, linear_balance_gains, pan_mono_to_stereo, AudioProcessor,
    GainProcessor, ParameterSmoother, StereoBalanceProcessor,
};

#[cfg(test)]
mod tests;

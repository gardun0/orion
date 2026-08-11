use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::AudioError;

/// How a channel's audio is mapped between its endpoint and the mix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelMode {
    /// Follow the endpoint's channel layout (default).
    #[default]
    Auto,
    /// Force stereo passthrough.
    Stereo,
    /// Downmix to mono (L+R)/2 on every channel.
    Mono,
    /// Use only the left channel.
    Left,
    /// Use only the right channel.
    Right,
    /// Swap left and right.
    Swap,
}

impl ChannelMode {
    pub const ALL: [Self; 6] = [
        Self::Auto,
        Self::Stereo,
        Self::Mono,
        Self::Left,
        Self::Right,
        Self::Swap,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "AUTO",
            Self::Stereo => "ST",
            Self::Mono => "MN",
            Self::Left => "L",
            Self::Right => "R",
            Self::Swap => "SW",
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Auto => Self::Stereo,
            Self::Stereo => Self::Mono,
            Self::Mono => Self::Left,
            Self::Left => Self::Right,
            Self::Right => Self::Swap,
            Self::Swap => Self::Auto,
        }
    }

    pub const fn code(self) -> u32 {
        self as u32
    }

    pub fn from_code(code: u32) -> Self {
        Self::ALL.get(code as usize).copied().unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct GainDb(f32);

impl GainDb {
    pub const MIN: f32 = -60.0;
    pub const MAX: f32 = 10.0;

    pub fn new(value: f32) -> Result<Self, AudioError> {
        if value.is_finite() && (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(AudioError::invalid_control(
                "gain",
                value,
                Self::MIN,
                Self::MAX,
            ))
        }
    }

    pub const fn value(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for GainDb {
    type Error = AudioError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<GainDb> for f32 {
    fn from(value: GainDb) -> Self {
        value.value()
    }
}

impl Default for GainDb {
    fn default() -> Self {
        Self(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct NormalizedBalance(f32);

impl NormalizedBalance {
    pub const MIN: f32 = -1.0;
    pub const MAX: f32 = 1.0;

    pub fn new(value: f32) -> Result<Self, AudioError> {
        if value.is_finite() && (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(AudioError::invalid_control(
                "balance",
                value,
                Self::MIN,
                Self::MAX,
            ))
        }
    }

    pub const fn value(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for NormalizedBalance {
    type Error = AudioError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<NormalizedBalance> for f32 {
    fn from(value: NormalizedBalance) -> Self {
        value.value()
    }
}

impl Default for NormalizedBalance {
    fn default() -> Self {
        Self(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct MeterLevel(f32);

impl MeterLevel {
    pub const MIN: f32 = 0.0;
    pub const MAX: f32 = 1.0;

    pub fn new(value: f32) -> Result<Self, AudioError> {
        if value.is_finite() && (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(AudioError::invalid_control(
                "meter level",
                value,
                Self::MIN,
                Self::MAX,
            ))
        }
    }

    pub const fn value(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for MeterLevel {
    type Error = AudioError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<MeterLevel> for f32 {
    fn from(value: MeterLevel) -> Self {
        value.value()
    }
}

impl Default for MeterLevel {
    fn default() -> Self {
        Self(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_accepts_inclusive_boundaries() {
        assert!(GainDb::new(GainDb::MIN).is_ok());
        assert!(GainDb::new(GainDb::MAX).is_ok());
    }

    #[test]
    fn controls_reject_out_of_range_and_non_finite_values() {
        assert!(GainDb::new(GainDb::MIN - 0.1).is_err());
        assert!(GainDb::new(f32::NAN).is_err());
        assert!(NormalizedBalance::new(1.01).is_err());
        assert!(MeterLevel::new(f32::INFINITY).is_err());
    }
}

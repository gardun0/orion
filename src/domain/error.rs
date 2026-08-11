use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidControlValue,
    EndpointNotFound,
    RouteNotFound,
    DuplicateEndpoint,
    DuplicateRoute,
    InvalidRoute,
    BackendUnavailable,
    PersistenceFailed,
    UnsupportedOperation,
    ChannelClosed,
    ThreadFailure,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{technical_message}")]
pub struct AudioError {
    pub code: ErrorCode,
    pub severity: ErrorSeverity,
    pub retryable: bool,
    pub user_message: String,
    pub technical_message: String,
}

impl AudioError {
    pub fn new(
        code: ErrorCode,
        severity: ErrorSeverity,
        retryable: bool,
        user_message: impl Into<String>,
        technical_message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            retryable,
            user_message: user_message.into(),
            technical_message: technical_message.into(),
        }
    }

    pub fn invalid_control(name: &str, value: f32, minimum: f32, maximum: f32) -> Self {
        Self::new(
            ErrorCode::InvalidControlValue,
            ErrorSeverity::Warning,
            false,
            format!("The {name} value is outside its supported range."),
            format!("invalid {name} value {value}; expected {minimum}..={maximum}"),
        )
    }

    pub fn channel_closed(channel: &str) -> Self {
        Self::new(
            ErrorCode::ChannelClosed,
            ErrorSeverity::Error,
            true,
            "The audio engine is unavailable.",
            format!("{channel} channel is closed"),
        )
    }

    pub fn thread_failure(thread: &str, detail: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::ThreadFailure,
            ErrorSeverity::Critical,
            false,
            "The audio engine could not finish that operation.",
            format!("{thread} thread failure: {}", detail.into()),
        )
    }
}

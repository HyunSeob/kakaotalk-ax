use std::fmt;

/// Unified error type matching the Python/Swift compatibility contract.
#[derive(Debug, Clone)]
pub enum KaxError {
    /// Corresponds to Python `ValueError` (validation failures, busy locks, payload conflicts).
    Validation(String),
    /// Expected safety stop with a stable public `reason` code.
    Blocked(String),
    /// Unexpected OS / runtime / IO error (redacted in CLI output).
    Runtime {
        error_type: String,
        detail: String,
    },
}

impl KaxError {
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }

    pub fn blocked(reason: impl Into<String>) -> Self {
        Self::Blocked(reason.into())
    }

    pub fn runtime(error_type: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Runtime {
            error_type: error_type.into(),
            detail: detail.into(),
        }
    }

    pub fn error_type(&self) -> &str {
        match self {
            Self::Validation(_) => "ValueError",
            Self::Blocked(_) => "Blocked",
            Self::Runtime { error_type, .. } => error_type.as_str(),
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Validation(m) | Self::Blocked(m) => m.as_str(),
            Self::Runtime { detail, .. } => detail.as_str(),
        }
    }
}

impl fmt::Display for KaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for KaxError {}

impl From<std::io::Error> for KaxError {
    fn from(err: std::io::Error) -> Self {
        Self::runtime("OSError", err.to_string())
    }
}

impl From<serde_json::Error> for KaxError {
    fn from(err: serde_json::Error) -> Self {
        Self::validation(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, KaxError>;

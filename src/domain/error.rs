use thiserror::Error;

/// Stable error codes shared by the CLI (exit codes) and MCP (structured errors).
#[derive(Debug, Error, PartialEq)]
pub enum AppError {
    #[error("{what} not found: {query}")]
    NotFound { what: &'static str, query: String },
    #[error("'{query}' is ambiguous, matches: {}", candidates.join(", "))]
    Ambiguous {
        query: String,
        candidates: Vec<String>,
    },
    #[error("invalid {field}: {reason}")]
    Invalid { field: &'static str, reason: String },
    #[error("confirmation required: {0}")]
    ConfirmationRequired(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("database is busy, try again")]
    Busy,
    #[error("storage error: {0}")]
    Storage(String),
}

pub type Result<T> = std::result::Result<T, AppError>;

impl AppError {
    #[cfg(feature = "mcp")]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not_found",
            Self::Ambiguous { .. } => "ambiguous",
            Self::Invalid { .. } => "invalid",
            Self::ConfirmationRequired(_) => "confirmation_required",
            Self::Conflict(_) => "conflict",
            Self::Busy => "busy",
            Self::Storage(_) => "storage",
        }
    }

    #[cfg(feature = "mcp")]
    /// Structured form for machine clients: `{code, message, field?, candidates?}`.
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({ "code": self.code(), "message": self.to_string() });
        match self {
            Self::Invalid { field, .. } => v["field"] = (*field).into(),
            Self::Ambiguous { candidates, .. } => v["candidates"] = candidates.clone().into(),
            _ => {}
        }
        v
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotFound { .. } => 2,
            Self::Ambiguous { .. } => 3,
            Self::Invalid { .. } => 4,
            Self::ConfirmationRequired(_) => 5,
            Self::Conflict(_) => 6,
            Self::Busy => 7,
            Self::Storage(_) => 1,
        }
    }

    pub fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::Invalid {
            field,
            reason: reason.into(),
        }
    }
}

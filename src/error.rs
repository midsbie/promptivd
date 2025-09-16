use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("HTTP server error: {0}")]
    Http(#[from] hyper::Error),

    #[error("WebSocket error: {0}")]
    WebSocket(#[from] axum::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("No sink connected")]
    NoSink,

    #[error("Invalid request: {reason}")]
    InvalidRequest { reason: String },

    #[error("Job payload too large: {size} bytes (max: {max})")]
    PayloadTooLarge { size: usize, max: usize },

    #[error("Sink registration failed: {reason}")]
    SinkRegistrationFailed { reason: String },

    #[error("Job dispatch timeout after {timeout_ms}ms")]
    DispatchTimeout { timeout_ms: u64 },
}

#[derive(Error, Debug)]
pub enum ValidationError {
    #[error("Missing required field: {field}")]
    MissingField { field: String },

    #[error("Invalid schema version: {version}")]
    InvalidSchemaVersion { version: String },

    #[error("Empty snippet content")]
    EmptySnippet,
}

pub type AppResult<T> = Result<T, AppError>;
pub type ValidationResult<T> = Result<T, ValidationError>;

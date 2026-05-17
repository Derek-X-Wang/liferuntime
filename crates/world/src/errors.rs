use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization: {0}")]
    Json(#[from] serde_json::Error),

    #[error("event log: {0}")]
    EventLog(#[from] liferuntime_event_log::JsonlError),

    #[error("invalid event: {0}")]
    InvalidEvent(String),

    #[error("entity {kind} with id '{id}' already exists")]
    DuplicateEntity { kind: &'static str, id: String },

    #[error("entity {kind} with id '{id}' not found")]
    EntityNotFound { kind: &'static str, id: String },

    #[error("value out of range for `{field}`: {value} (expected {min}..={max})")]
    ValueOutOfRange {
        field: &'static str,
        value: f32,
        min: f32,
        max: f32,
    },
}

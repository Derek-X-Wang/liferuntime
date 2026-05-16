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
}

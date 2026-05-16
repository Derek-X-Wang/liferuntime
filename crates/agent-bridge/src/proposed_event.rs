use serde::{Deserialize, Serialize};

/// An event suggested by an [`crate::AgentBridge`] adapter.
///
/// The world runtime is responsible for deciding whether to ingest this.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposedEvent {
    pub source: String,
    pub summary: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub confidence: f32,
    pub rationale: String,
}

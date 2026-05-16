use serde::{Deserialize, Serialize};

use crate::proposed_event::ProposedEvent;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignalAnalysisInput {
    pub text: String,
    #[serde(default)]
    pub hints: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorldSummaryInput {
    pub recent_changes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScenarioInput {
    pub focus: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Narrative(pub String);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub description: String,
    pub probability: f32,
}

/// The seam between the deterministic runtime and probabilistic providers.
///
/// Adapters return *proposals*. The world runtime is responsible for
/// validating and ingesting them. Adapters must never reach inside the
/// world runtime.
pub trait AgentBridge {
    type Error: std::error::Error + Send + Sync + 'static;

    fn analyze_signal(
        &self,
        input: SignalAnalysisInput,
    ) -> Result<Vec<ProposedEvent>, Self::Error>;

    fn summarize_world(&self, input: WorldSummaryInput) -> Result<Narrative, Self::Error>;

    fn propose_scenarios(&self, input: ScenarioInput) -> Result<Vec<Scenario>, Self::Error>;
}

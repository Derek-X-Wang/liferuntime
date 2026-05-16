//! Boundary between the deterministic world runtime and probabilistic AI
//! providers.
//!
//! AI may *propose* events, summaries, or scenarios. The runtime decides
//! whether to ingest them. No adapter in this crate is allowed to mutate
//! world state directly. The [`FakeAgent`] adapter is the only thing
//! shipped today; OpenAI / Anthropic / local model adapters are deferred
//! until a real product seam justifies them (see ADR-0001).

mod fake;
mod proposed_event;
mod provider;

pub use fake::FakeAgent;
pub use proposed_event::ProposedEvent;
pub use provider::{
    AgentBridge, Narrative, Scenario, ScenarioInput, SignalAnalysisInput, WorldSummaryInput,
};

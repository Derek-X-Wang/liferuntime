use serde::{Deserialize, Serialize};

use crate::proposed_event::ProposedEvent;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignalAnalysisInput {
    pub text: String,
    #[serde(default)]
    pub hints: Vec<String>,
}

/// The seam between the deterministic runtime and probabilistic providers.
///
/// Adapters return *proposals*. The world runtime is responsible for
/// validating and ingesting them. Adapters must never reach inside the
/// world runtime.
///
/// The trait is intentionally **single-method**: one adapter ([`FakeAgent`])
/// exists today, the call site is `liferuntime signal analyze`, and
/// per ADR-0001 / `docs/LANGUAGE.md` we resist adding `summarize_world` /
/// `propose_scenarios` until a real adapter and a real call site materialize.
///
/// [`FakeAgent`]: crate::FakeAgent
pub trait AgentBridge {
    type Error: std::error::Error + Send + Sync + 'static;

    fn analyze_signal(&self, input: SignalAnalysisInput)
        -> Result<Vec<ProposedEvent>, Self::Error>;
}

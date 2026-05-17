//! Boundary between the deterministic world runtime and probabilistic AI
//! providers.
//!
//! AI may *propose* events. The runtime decides whether to ingest them. No
//! adapter in this crate is allowed to mutate world state directly.
//!
//! v1 ships:
//!   - one trait method: [`AgentBridge::analyze_signal`]
//!   - one adapter: [`FakeAgent`]
//!   - one call site: `liferuntime signal analyze` in the CLI
//!
//! Methods like `summarize_world` / `propose_scenarios` were trimmed from
//! the trait until a real adapter and a real consumer justify them
//! (one adapter = hypothetical seam; see `docs/LANGUAGE.md`).

mod fake;
mod proposed_event;
mod provider;

pub use fake::FakeAgent;
pub use proposed_event::ProposedEvent;
pub use provider::{AgentBridge, SignalAnalysisInput};

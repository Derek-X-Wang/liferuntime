//! The deterministic core of LifeRuntime.
//!
//! [`WorldRuntime`] is the single deep module exposed by this crate. Callers
//! interact with it as: ingest events, advance the world, query state,
//! explain changes. The ECS implementation, system ordering, change-log
//! mechanics, and persistence layout are all internal.
//!
//! AI providers do not live here — they live behind the
//! `liferuntime-agent-bridge` seam and propose events which the runtime
//! validates and ingests.

pub mod errors;
pub mod events;
pub mod explanation;
pub mod model;
pub mod queries;
pub mod runtime;
pub mod systems;

pub use errors::RuntimeError;
pub use events::WorldEvent;
pub use explanation::{Cause, ChangeRecord, ExplainTarget, Explanation};
pub use model::{
    Goal, GoalStatus, Identity, LastTouched, LatestEventId, Now, Project, ProjectStatus, Signal,
};
pub use queries::{GoalView, ProjectTrajectoryView, ProjectView};
pub use runtime::{IngestReceipt, WorldChanges, WorldRuntime};

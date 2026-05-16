use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Public events that can be ingested into the world.
///
/// Derived events (system outputs like `ProjectUpdated`) are not in this
/// enum — they are surfaced as [`crate::ChangeRecord`]s through
/// [`crate::WorldRuntime::advance`]. The log only stores ingested inputs;
/// derived state is rebuilt by re-running systems on replay. That is what
/// makes replay deterministic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum WorldEvent {
    ProjectCreated {
        id: String,
        name: String,
        #[serde(default)]
        tags: Vec<String>,
    },
    GoalCreated {
        id: String,
        name: String,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default = "default_importance")]
        importance: f32,
    },
    SignalObserved {
        source: String,
        summary: String,
        #[serde(default)]
        tags: Vec<String>,
        confidence: f32,
        #[serde(default)]
        observed_at: Option<DateTime<Utc>>,
    },
}

fn default_importance() -> f32 {
    0.5
}

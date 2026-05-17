use bevy_ecs::prelude::*;
use chrono::{DateTime, Utc};
use liferuntime_event_log::EventId;
use serde::{Deserialize, Serialize};

/// Stable external identifier for an entity (Project id, Goal id, ...).
#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct Identity(pub String);

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub tags: Vec<String>,
    pub strategic_relevance: f32,
    pub urgency: f32,
    pub momentum: f32,
    pub maintenance_burden: f32,
}

impl Project {
    pub fn new(name: impl Into<String>, tags: Vec<String>) -> Self {
        Self {
            name: name.into(),
            tags,
            strategic_relevance: 0.5,
            urgency: 0.4,
            momentum: 0.4,
            maintenance_burden: 0.3,
        }
    }
}

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct Goal {
    pub name: String,
    pub tags: Vec<String>,
    pub importance: f32,
}

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct Signal {
    pub triggering_event_id: EventId,
    pub source: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub confidence: f32,
    pub observed_at: DateTime<Utc>,
}

/// Marker placed on a Signal entity when first ingested. Systems remove it
/// after processing so the same signal is not re-applied in the same
/// in-memory session.
#[derive(Component, Default)]
pub struct Unprocessed;

/// Records the last time (in event-log time) that a Project was *touched*
/// by a relevant Signal — i.e. that the matching system found tag
/// overlap. Used by the decay system to compute how stale the Project
/// has become.
#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct LastTouched {
    pub at: DateTime<Utc>,
}

/// The runtime's notion of "now" — the timestamp of the most recent
/// Event in the log. Stored as a Bevy resource so systems can read it
/// without touching the wall clock (ADR-0004).
#[derive(Resource, Clone, Debug)]
pub struct Now(pub DateTime<Utc>);

impl Now {
    pub fn at(&self) -> DateTime<Utc> {
        self.0
    }
}

impl Default for Now {
    fn default() -> Self {
        Self(chrono::DateTime::<Utc>::from_timestamp(0, 0).expect("epoch valid"))
    }
}

/// The id of the most recent Event in the log. Systems that emit
/// ChangeRecords driven by "time passing" (e.g. Decay) tag the records
/// with this id so the cursor-based delta filter in
/// [`crate::WorldRuntime::advance`] still works:
///   - If `latest > cursor`, new events have arrived → decay records pass.
///   - If `latest == cursor`, the log hasn't moved → decay records are
///     filtered out, even if the schedule produced them again.
#[derive(Resource, Clone, Debug, Default)]
pub struct LatestEventId(pub Option<liferuntime_event_log::EventId>);

impl LatestEventId {
    pub fn get(&self) -> Option<&liferuntime_event_log::EventId> {
        self.0.as_ref()
    }
}

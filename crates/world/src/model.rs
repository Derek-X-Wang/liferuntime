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
    pub status: ProjectStatus,
    #[serde(default)]
    pub archived_reason: Option<String>,
    #[serde(default)]
    pub completion_note: Option<String>,
}

impl Project {
    pub fn new(name: impl Into<String>, tags: Vec<String>) -> Self {
        Self {
            name: name.into(),
            tags,
            strategic_relevance: 0.5,
            urgency: 0.5,
            status: ProjectStatus::Active,
            archived_reason: None,
            completion_note: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    #[default]
    Active,
    Archived,
    Completed,
}

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct Goal {
    pub name: String,
    pub tags: Vec<String>,
    pub importance: f32,
    #[serde(default)]
    pub status: GoalStatus,
    #[serde(default)]
    pub achievement_note: Option<String>,
    #[serde(default)]
    pub abandonment_reason: Option<String>,
}

impl Goal {
    pub fn new(name: impl Into<String>, tags: Vec<String>, importance: f32) -> Self {
        Self {
            name: name.into(),
            tags,
            importance,
            status: GoalStatus::Active,
            achievement_note: None,
            abandonment_reason: None,
        }
    }
}

/// Goals have a value-charged lifecycle distinct from Projects:
/// - **Active** — the goal still pulls on strategy; amplifies matching.
/// - **Achieved** — the goal was reached. Historical record.
/// - **Abandoned** — the goal was given up. Historical record.
///
/// Both terminal states stop the goal's amplification of matching.
/// Reactivation moves it back to Active. The achieved/abandoned
/// distinction matters for future systems (recommendations after a win
/// look different from recommendations after a loss).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    #[default]
    Active,
    Achieved,
    Abandoned,
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

/// Canonicalize a tag for matching purposes: lowercase, trim, collapse
/// space / underscore to dash. So `"AI Voice"`, `"ai-voice"`, and
/// `" Ai_voice "` all canonicalize to `"ai-voice"` and match each
/// other in the matching/amplifier systems.
///
/// Storage keeps the user's original spelling (for display). Comparison
/// uses this canonical form. Future synonym handling (aliases) could
/// layer on top of this.
pub fn canonical_tag(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.trim().chars() {
        match ch {
            ' ' | '_' => out.push('-'),
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

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

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

/// Derived per-Project stance imposed by the **most-recent Decision per
/// project** (ADR-0008 `#per-project-stance-derived-by-replay`).
///
/// Absence of the component on a Project entity means "no Decision
/// currently steers this project." `Chosen` and `Dampened` are mutually
/// exclusive — a later Decision flips one to the other in place.
///
/// This slice (issue #2) carries the stance shape only; the decaying
/// boost (issue #4) and the matching-side dampening (issue #5) are
/// layered on top in later slices.
#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionStance {
    Chosen { decision_id: EventId },
    Dampened { decision_id: EventId },
}

impl DecisionStance {
    /// The id of the Decision that currently steers a project.
    pub fn decision_id(&self) -> &EventId {
        match self {
            Self::Chosen { decision_id } | Self::Dampened { decision_id } => decision_id,
        }
    }
}

/// Transient component spawned by `apply_event` for every
/// [`crate::WorldEvent::DecisionRecorded`]. The
/// `decision_application_system` consumes pending decisions during the
/// next schedule run, flips the targeted Project stances, then despawns
/// the marker. Replay rebuilds the same final per-project stance
/// because per-event scheduling (ADR-0006) re-runs the system after
/// every event in order.
#[derive(Component, Clone, Debug)]
pub struct PendingDecision {
    pub decision_id: EventId,
    pub chose: String,
    pub dampen: Vec<String>,
}

/// Decaying boost on visible `strategic_relevance` attached to a
/// Project entity for the duration of a `Chosen` Decision stance
/// (ADR-0008 `#chosen-decaying-boost-not-a-floor`).
///
/// Stored separately from `Project.strategic_relevance` so the raw
/// value the matching/decay systems see is **never** touched by a
/// Decision. The boost is an additive layer surfaced through
/// `ProjectView::strategic_relevance_visible`.
///
/// `last_decay_at` tracks the event-log time at which the boost was
/// last advanced, so the `decision_boost_decay_system` can apply
/// `0.999^days_elapsed` per per-event tick without compounding from
/// the initial bump.
///
/// Removed when:
/// - The owning Decision is revoked (issue #3).
/// - A later Decision flips the project's stance to `Dampened` or
///   away from `Chosen` (issue #6 covers the full transition matrix).
#[derive(Component, Clone, Debug)]
pub struct DecisionBoost {
    pub decision_id: EventId,
    pub remaining: f32,
    pub last_decay_at: DateTime<Utc>,
}

impl DecisionBoost {
    /// Initial boost magnitude on `Chosen` (ADR-0008).
    pub const INITIAL: f32 = 0.15;
    /// Per-event-log-day decay factor (ADR-0008).
    pub const DECAY_PER_DAY: f32 = 0.999;
}

/// Set of decision_ids that have been observed in `DecisionRecorded`
/// events during the lifetime of this runtime. Rebuilt by replay from
/// the event log.
///
/// Two consumers:
///   1. `validate_event` for `DecisionRevoked` — reject loudly at
///      ingest if the referenced id was never recorded.
///   2. `apply_event` for `DecisionRevoked` — silently no-op if the
///      id is unknown (replay tolerance for a corrupted log; see
///      ADR-0008 `#lifecycle` amendment).
#[derive(Resource, Default, Clone, Debug)]
pub struct RecordedDecisions(pub std::collections::HashSet<EventId>);

impl RecordedDecisions {
    pub fn contains(&self, id: &EventId) -> bool {
        self.0.contains(id)
    }

    pub fn insert(&mut self, id: EventId) {
        self.0.insert(id);
    }
}

use serde::{Deserialize, Serialize};

use crate::model::ProjectStatus;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub tags: Vec<String>,
    pub strategic_relevance: f32,
    pub urgency: f32,
    pub status: ProjectStatus,
    pub archived_reason: Option<String>,
    pub completion_note: Option<String>,
}

/// One entry in the output of [`crate::WorldRuntime::trajectories`]:
/// a Project's current values plus the net change in
/// `strategic_relevance` during the most recent Advance that touched
/// this Project.
///
/// The most-recent-delta semantic (rather than a window-wide average)
/// is intentional: status commands want "what shifted just now",
/// not "what averaged out over the last month". A bump followed by a
/// decay should show ↓ cooling, not ~zero stable.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectTrajectoryView {
    pub id: String,
    pub name: String,
    pub status: ProjectStatus,
    pub current_relevance: f32,
    pub current_urgency: f32,
    /// Net change in `strategic_relevance` during the most recent
    /// Advance (within the observation window) that produced any
    /// record for this Project. Positive → warming; negative → cooling.
    /// Zero if no Advance in the window touched this Project.
    pub slope_relevance: f32,
    /// Number of Advances within the window where this Project saw a
    /// strategic_relevance change. Zero means no recent activity.
    pub advances_observed: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalView {
    pub id: String,
    pub name: String,
    pub tags: Vec<String>,
    pub importance: f32,
}

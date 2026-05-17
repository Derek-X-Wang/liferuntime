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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalView {
    pub id: String,
    pub name: String,
    pub tags: Vec<String>,
    pub importance: f32,
}

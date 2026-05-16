use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub tags: Vec<String>,
    pub strategic_relevance: f32,
    pub urgency: f32,
    pub momentum: f32,
    pub maintenance_burden: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalView {
    pub id: String,
    pub name: String,
    pub tags: Vec<String>,
    pub importance: f32,
}

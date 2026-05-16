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

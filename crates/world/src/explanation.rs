use bevy_ecs::prelude::*;
use liferuntime_event_log::EventId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// In-memory accumulator for every change that systems make.
///
/// Cleared at the start of each [`crate::WorldRuntime::advance`] so the
/// records left behind are exactly the changes derived in that call.
#[derive(Resource, Default, Debug, Clone)]
pub struct ChangeLog {
    pub records: Vec<ChangeRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangeRecord {
    pub triggered_by_event: EventId,
    pub entity_id: String,
    pub field: String,
    pub before: f32,
    pub after: f32,
    pub causes: Vec<Cause>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Cause {
    SignalMatched {
        signal_summary: String,
        matched_tags: Vec<String>,
        confidence: f32,
    },
    /// Time-based drift back toward baseline because no recent Signal
    /// matched this Project. `days_elapsed` is in event-log days, not
    /// wall-clock (ADR-0004).
    Decay { days_elapsed: f32 },
    /// A high-importance Goal in the same tag-neighborhood amplified the
    /// signal's effect on this Project. `factor` is the multiplier
    /// applied (e.g. 1.45 means a 45% boost over the base delta).
    GoalAmplified {
        goal_id: String,
        goal_name: String,
        importance: f32,
        factor: f32,
    },
}

#[derive(Clone, Debug)]
pub enum ExplainTarget {
    /// The records emitted by the most recent advance.
    LatestChange,
    /// All records that touched the given entity.
    Entity(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Explanation {
    pub records: Vec<ChangeRecord>,
}

impl Explanation {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

impl fmt::Display for Explanation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.records.is_empty() {
            return write!(f, "No changes to explain.");
        }

        let mut by_entity: BTreeMap<&str, Vec<&ChangeRecord>> = BTreeMap::new();
        for r in &self.records {
            by_entity.entry(r.entity_id.as_str()).or_default().push(r);
        }

        let mut first_entity = true;
        for (entity, records) in by_entity {
            if !first_entity {
                writeln!(f)?;
            }
            first_entity = false;

            writeln!(f, "Entity: {entity}")?;
            writeln!(f, "Changes:")?;
            for r in &records {
                writeln!(f, "  - {}: {:.2} → {:.2}", r.field, r.before, r.after)?;
            }
            writeln!(f, "Why:")?;
            let mut seen: BTreeSet<String> = BTreeSet::new();
            for r in &records {
                for c in &r.causes {
                    let rendered = render_cause(c);
                    if seen.insert(rendered.clone()) {
                        writeln!(f, "  - {rendered}")?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn render_cause(c: &Cause) -> String {
    match c {
        Cause::SignalMatched {
            signal_summary,
            matched_tags,
            confidence,
        } => format!(
            "Signal \"{}\" matched on tags [{}], confidence {:.2}",
            signal_summary,
            matched_tags.join(", "),
            confidence,
        ),
        Cause::Decay { days_elapsed } => {
            format!("Decay: {:.1} days since last relevant signal", days_elapsed,)
        }
        Cause::GoalAmplified {
            goal_name,
            importance,
            factor,
            ..
        } => format!(
            "Goal \"{}\" (importance {:.2}) amplified by ×{:.2}",
            goal_name, importance, factor,
        ),
    }
}

use bevy_ecs::prelude::*;

use crate::explanation::{Cause, ChangeLog, ChangeRecord};
use crate::model::{Identity, Project, Signal, Unprocessed};

pub fn register_systems(schedule: &mut Schedule) {
    schedule.add_systems(signal_project_matching_system);
}

/// For each unprocessed signal, raise the strategic relevance and urgency of
/// every project whose tags overlap the signal's tags. The amount of the
/// bump is `overlap_fraction * confidence * weight`. The signal's marker is
/// removed so it is not re-applied within this in-memory session.
pub fn signal_project_matching_system(
    mut commands: Commands,
    signals: Query<(Entity, &Signal), With<Unprocessed>>,
    mut projects: Query<(&Identity, &mut Project)>,
    mut change_log: ResMut<ChangeLog>,
) {
    for (signal_entity, signal) in &signals {
        for (project_id, mut project) in &mut projects {
            let matched: Vec<String> = signal
                .tags
                .iter()
                .filter(|t| {
                    project
                        .tags
                        .iter()
                        .any(|pt| pt.eq_ignore_ascii_case(t))
                })
                .cloned()
                .collect();
            if matched.is_empty() {
                continue;
            }

            let breadth = project.tags.len().max(1) as f32;
            let overlap = matched.len() as f32 / breadth;
            let relevance_delta = overlap * signal.confidence * 0.25;
            let urgency_delta = overlap * signal.confidence * 0.15;

            let cause = Cause::SignalMatched {
                signal_summary: signal.summary.clone(),
                matched_tags: matched.clone(),
                confidence: signal.confidence,
            };

            let before_rel = project.strategic_relevance;
            project.strategic_relevance =
                (project.strategic_relevance + relevance_delta).clamp(0.0, 1.0);
            change_log.records.push(ChangeRecord {
                triggered_by_event: signal.triggering_event_id.clone(),
                entity_id: project_id.0.clone(),
                field: "strategic_relevance".into(),
                before: before_rel,
                after: project.strategic_relevance,
                causes: vec![cause.clone()],
            });

            let before_urg = project.urgency;
            project.urgency = (project.urgency + urgency_delta).clamp(0.0, 1.0);
            change_log.records.push(ChangeRecord {
                triggered_by_event: signal.triggering_event_id.clone(),
                entity_id: project_id.0.clone(),
                field: "urgency".into(),
                before: before_urg,
                after: project.urgency,
                causes: vec![cause],
            });
        }
        commands.entity(signal_entity).remove::<Unprocessed>();
    }
}

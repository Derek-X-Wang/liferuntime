use bevy_ecs::prelude::*;

use crate::explanation::{Cause, ChangeLog, ChangeRecord};
use crate::model::{Identity, LastTouched, LatestEventId, Now, Project, Signal, Unprocessed};

pub fn register_systems(schedule: &mut Schedule) {
    // Matching runs first so freshly-arrived signals update LastTouched
    // before decay reads it. Decay therefore sees `days_elapsed = 0` for
    // any project just touched, leaving the match's effect intact.
    schedule.add_systems((signal_project_matching_system, project_decay_system).chain());
}

/// For each unprocessed signal, raise the strategic relevance and urgency of
/// every project whose tags overlap the signal's tags. Mark the project's
/// `LastTouched` to the signal's observed time so decay knows the project
/// is fresh.
pub fn signal_project_matching_system(
    mut commands: Commands,
    signals: Query<(Entity, &Signal), With<Unprocessed>>,
    mut projects: Query<(Entity, &Identity, &mut Project)>,
    mut change_log: ResMut<ChangeLog>,
) {
    for (signal_entity, signal) in &signals {
        for (project_entity, project_id, mut project) in &mut projects {
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

            commands
                .entity(project_entity)
                .insert(LastTouched { at: signal.observed_at });
        }
        commands.entity(signal_entity).remove::<Unprocessed>();
    }
}

/// Drift Project fields toward baseline over event-log time. Triggered by
/// the "freshest" Event in the log (read via `Now`), not the wall clock —
/// so replay is deterministic (ADR-0004).
///
/// Formula: `field = baseline + (field - baseline) * factor.powf(days)`.
/// `factor = 0.95` per day → roughly 5% closer to baseline per quiet day.
pub fn project_decay_system(
    now: Res<Now>,
    latest_event: Res<LatestEventId>,
    mut projects: Query<(&Identity, &mut Project, &LastTouched)>,
    mut change_log: ResMut<ChangeLog>,
) {
    const BASELINE: f32 = 0.5;
    const FACTOR_PER_DAY: f32 = 0.95;
    const SECS_PER_DAY: f32 = 86_400.0;

    let Some(triggering_id) = latest_event.get().cloned() else {
        return; // empty log → no time has passed → nothing to decay
    };

    for (project_id, mut project, last_touched) in &mut projects {
        let elapsed_secs = (now.at() - last_touched.at).num_seconds().max(0) as f32;
        let days = elapsed_secs / SECS_PER_DAY;
        if days <= 0.0 {
            continue;
        }
        let pull = FACTOR_PER_DAY.powf(days);

        apply_decay(
            &mut change_log,
            &triggering_id,
            project_id,
            "strategic_relevance",
            &mut project.strategic_relevance,
            BASELINE,
            pull,
            days,
        );
        apply_decay(
            &mut change_log,
            &triggering_id,
            project_id,
            "urgency",
            &mut project.urgency,
            BASELINE,
            pull,
            days,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_decay(
    change_log: &mut ChangeLog,
    triggering_id: &liferuntime_event_log::EventId,
    project_id: &Identity,
    field: &'static str,
    field_value: &mut f32,
    baseline: f32,
    pull: f32,
    days: f32,
) {
    let before = *field_value;
    let after = baseline + (before - baseline) * pull;
    if (after - before).abs() < 1e-4 {
        return;
    }
    *field_value = after;
    change_log.records.push(ChangeRecord {
        triggered_by_event: triggering_id.clone(),
        entity_id: project_id.0.clone(),
        field: field.into(),
        before,
        after,
        causes: vec![Cause::Decay { days_elapsed: days }],
    });
}

use bevy_ecs::prelude::*;

use crate::explanation::{Cause, ChangeLog, ChangeRecord};
use crate::model::{
    canonical_tag, DecisionBoost, DecisionStance, Goal, GoalStatus, Identity, LastTouched,
    LatestEventId, Now, PendingDecision, Project, ProjectStatus, Signal, Unprocessed,
};

pub fn register_systems(schedule: &mut Schedule) {
    // decision_application runs first so any subsequent matching /
    // decay step in this same per-event tick sees the stance the user
    // just declared (issues #4/#5 lean on this ordering for the boost
    // and dampening multipliers). decision_boost_decay runs next so a
    // TimePulse erodes the additive boost layer before matching reads
    // the project's visible relevance for downstream views. Matching
    // runs before project_decay so freshly-arrived signals update
    // LastTouched before decay reads it; decay then sees
    // `days_elapsed = 0` for any project just touched, leaving the
    // match's effect intact.
    schedule.add_systems(
        (
            decision_application_system,
            decision_boost_decay_system,
            signal_project_matching_system,
            project_decay_system,
        )
            .chain(),
    );
}

/// Apply a queued [`PendingDecision`] to every targeted Project's
/// stance, then despawn the marker.
///
/// ADR-0008 `#per-project-stance-derived-by-replay`: "most-recent
/// Decision wins per project" by replay order. Bevy's
/// `commands.entity(...).insert(component)` *replaces* an existing
/// component of the same type, so this implementation naturally
/// supersedes the prior stance on each targeted Project.
///
/// `over` is intentionally **not** part of `PendingDecision`. Per
/// ADR-0008 it is narrative-only and has no mechanical effect; the
/// `decision list` command (issue #7) reconstructs it from the
/// originating event in the log by `decision_id`.
pub fn decision_application_system(
    mut commands: Commands,
    now: Res<Now>,
    pending: Query<(Entity, &PendingDecision)>,
    projects: Query<(Entity, &Identity, &Project, Option<&DecisionBoost>)>,
    mut change_log: ResMut<ChangeLog>,
) {
    for (pending_entity, decision) in &pending {
        for (proj_entity, ident, project, current_boost) in &projects {
            if ident.0 == decision.chose {
                // Issue #4: apply (or replace) the decaying boost.
                // Boost is `+0.15` initially and decays toward zero;
                // boosts do NOT stack — a later Chosen for the same
                // project resets the timer (#6 covers the transition
                // matrix in detail).
                let prior_contribution = current_boost.map(|b| b.remaining).unwrap_or(0.0);
                let visible_before =
                    (project.strategic_relevance + prior_contribution).clamp(0.0, 1.0);
                let visible_after =
                    (project.strategic_relevance + DecisionBoost::INITIAL).clamp(0.0, 1.0);

                commands.entity(proj_entity).insert(DecisionStance::Chosen {
                    decision_id: decision.decision_id.clone(),
                });
                commands.entity(proj_entity).insert(DecisionBoost {
                    decision_id: decision.decision_id.clone(),
                    remaining: DecisionBoost::INITIAL,
                    last_decay_at: now.at(),
                });

                if (visible_after - visible_before).abs() >= 1e-6 {
                    change_log.records.push(ChangeRecord {
                        triggered_by_event: decision.decision_id.clone(),
                        entity_id: ident.0.clone(),
                        field: "strategic_relevance".into(),
                        before: visible_before,
                        after: visible_after,
                        causes: vec![Cause::DecisionBoostApplied {
                            decision_id: decision.decision_id.clone(),
                            contribution: DecisionBoost::INITIAL,
                        }],
                    });
                }
            } else if decision.dampen.iter().any(|id| id == &ident.0) {
                commands
                    .entity(proj_entity)
                    .insert(DecisionStance::Dampened {
                        decision_id: decision.decision_id.clone(),
                    });
                // A Dampened project doesn't carry a boost. If a prior
                // Chosen Decision was steering it, drop the boost
                // component so visible falls back to raw.
                if current_boost.is_some() {
                    commands.entity(proj_entity).remove::<DecisionBoost>();
                }
            }
        }
        commands.entity(pending_entity).despawn();
    }
}

/// Erode each Project's `DecisionBoost` by `0.999 ^ days_elapsed` per
/// per-event tick (ADR-0008 `#chosen-decaying-boost-not-a-floor`).
///
/// Emits a `ChangeRecord` whose `before`/`after` capture the **visible**
/// strategic_relevance movement (raw + boost), citing
/// `Cause::DecisionBoostApplied` with the contribution remaining
/// after this tick. Raw is never touched.
pub fn decision_boost_decay_system(
    now: Res<Now>,
    latest_event: Res<LatestEventId>,
    mut boosted: Query<(&Identity, &Project, &mut DecisionBoost)>,
    mut change_log: ResMut<ChangeLog>,
) {
    const SECS_PER_DAY: f32 = 86_400.0;

    let Some(triggering_id) = latest_event.get().cloned() else {
        return; // empty log → no time has passed
    };

    for (ident, project, mut boost) in &mut boosted {
        let elapsed_secs = (now.at() - boost.last_decay_at).num_seconds().max(0) as f32;
        let days = elapsed_secs / SECS_PER_DAY;
        if days <= 0.0 {
            continue;
        }

        let new_remaining = boost.remaining * DecisionBoost::DECAY_PER_DAY.powf(days);
        let visible_before = (project.strategic_relevance + boost.remaining).clamp(0.0, 1.0);
        let visible_after = (project.strategic_relevance + new_remaining).clamp(0.0, 1.0);

        boost.remaining = new_remaining;
        boost.last_decay_at = now.at();

        // Skip emitting if the visible movement is negligible — e.g.
        // a sub-second per-event tick on an already-tiny boost. The
        // boost component still updates so the next non-trivial tick
        // computes from the correct baseline.
        if (visible_after - visible_before).abs() < 1e-4 {
            continue;
        }

        change_log.records.push(ChangeRecord {
            triggered_by_event: triggering_id.clone(),
            entity_id: ident.0.clone(),
            field: "strategic_relevance".into(),
            before: visible_before,
            after: visible_after,
            causes: vec![Cause::DecisionBoostApplied {
                decision_id: boost.decision_id.clone(),
                contribution: new_remaining,
            }],
        });
    }
}

/// For each unprocessed signal:
///   1. Compute a goal-amplification factor from any high-importance
///      Goal whose tags overlap the signal's tags.
///   2. For each active Project whose tags overlap the signal, raise
///      strategic_relevance and urgency by `overlap * confidence * 0.25 *
///      goal_amplifier` and `... * 0.15 * goal_amplifier` respectively.
///   3. Update the Project's `LastTouched`.
///   4. **Despawn the Signal entity** — signals are transient in the
///      ECS; the canonical signal history lives in the event log.
pub fn signal_project_matching_system(
    mut commands: Commands,
    signals: Query<(Entity, &Signal), With<Unprocessed>>,
    goals: Query<(&Identity, &Goal)>,
    mut projects: Query<(Entity, &Identity, &mut Project, Option<&DecisionStance>)>,
    mut change_log: ResMut<ChangeLog>,
) {
    /// Dampening factor on resonance deltas for `Dampened` projects
    /// (ADR-0008 `#dampened-x03-with-goal-amp-suppressed`).
    const DAMPENING_FACTOR: f32 = 0.3;

    let goal_snapshot: Vec<(Identity, Goal)> = goals
        .iter()
        .map(|(id, g)| (id.clone(), g.clone()))
        .collect();

    for (signal_entity, signal) in &signals {
        let (goal_factor, amplifier_cause) = goal_amplifier(&goal_snapshot, &signal.tags);

        for (project_entity, project_id, mut project, stance) in &mut projects {
            // Under per-event scheduling (ADR-0006), matching runs at
            // the moment a signal is ingested. Archive events that
            // arrive *after* this signal haven't happened yet from
            // matching's perspective, so the project is still Active
            // here. A simple status check is correct.
            if project.status != ProjectStatus::Active {
                continue;
            }
            // Compare canonical forms ("AI Voice" matches "ai-voice").
            // Keep the signal's original spelling in `matched` so the
            // explanation cites what the user actually wrote.
            let project_canonical: Vec<String> =
                project.tags.iter().map(|t| canonical_tag(t)).collect();
            let matched: Vec<String> = signal
                .tags
                .iter()
                .filter(|t| project_canonical.contains(&canonical_tag(t)))
                .cloned()
                .collect();
            if matched.is_empty() {
                continue;
            }

            let breadth = project.tags.len().max(1) as f32;
            let overlap = matched.len() as f32 / breadth;
            let base_relevance_delta = overlap * signal.confidence * 0.25;
            let base_urgency_delta = overlap * signal.confidence * 0.15;

            // ADR-0008 `#dampened-x03-with-goal-amp-suppressed`: if
            // the project is currently Dampened, scale by 0.3 AND
            // suppress goal amplification. Otherwise the existing
            // goal-amp behavior is unchanged.
            let (factor, extra_cause): (f32, Option<Cause>) = match stance {
                Some(DecisionStance::Dampened { decision_id }) => (
                    DAMPENING_FACTOR,
                    Some(Cause::DecisionDampened {
                        decision_id: decision_id.clone(),
                        factor: DAMPENING_FACTOR,
                    }),
                ),
                _ => (goal_factor, amplifier_cause.clone()),
            };

            let relevance_delta = base_relevance_delta * factor;
            let urgency_delta = base_urgency_delta * factor;

            let mut causes = vec![Cause::SignalMatched {
                signal_summary: signal.summary.clone(),
                matched_tags: matched.clone(),
                confidence: signal.confidence,
            }];
            if let Some(c) = extra_cause {
                causes.push(c);
            }

            let before_rel = project.strategic_relevance;
            project.strategic_relevance =
                (project.strategic_relevance + relevance_delta).clamp(0.0, 1.0);
            change_log.records.push(ChangeRecord {
                triggered_by_event: signal.triggering_event_id.clone(),
                entity_id: project_id.0.clone(),
                field: "strategic_relevance".into(),
                before: before_rel,
                after: project.strategic_relevance,
                causes: causes.clone(),
            });

            let before_urg = project.urgency;
            project.urgency = (project.urgency + urgency_delta).clamp(0.0, 1.0);
            change_log.records.push(ChangeRecord {
                triggered_by_event: signal.triggering_event_id.clone(),
                entity_id: project_id.0.clone(),
                field: "urgency".into(),
                before: before_urg,
                after: project.urgency,
                causes,
            });

            commands.entity(project_entity).insert(LastTouched {
                at: signal.observed_at,
            });
        }
        // Transient signals: the entity exists only for matching. After
        // processing, despawn — the canonical signal history is in the
        // event log (`events.jsonl`), not the ECS world.
        commands.entity(signal_entity).despawn();
    }
}

/// Compute the goal-amplification factor for a Signal: `1 + 0.5 *
/// max(importance)` over Goals whose tags overlap the signal's tags. If
/// no Goal overlaps, factor = 1.0 and no cause is produced.
fn goal_amplifier(goals: &[(Identity, Goal)], signal_tags: &[String]) -> (f32, Option<Cause>) {
    let mut best: Option<(&Identity, &Goal)> = None;
    for (id, goal) in goals {
        // Only Active goals amplify. Achieved / Abandoned goals are
        // historical records and don't pull on current strategy.
        if goal.status != GoalStatus::Active {
            continue;
        }
        let signal_canonical: Vec<String> = signal_tags.iter().map(|t| canonical_tag(t)).collect();
        let overlap = goal
            .tags
            .iter()
            .any(|gt| signal_canonical.contains(&canonical_tag(gt)));
        if !overlap {
            continue;
        }
        match best {
            None => best = Some((id, goal)),
            Some((_, prev)) if goal.importance > prev.importance => best = Some((id, goal)),
            _ => {}
        }
    }
    match best {
        None => (1.0, None),
        Some((id, goal)) => {
            let factor = 1.0 + 0.5 * goal.importance;
            (
                factor,
                Some(Cause::GoalAmplified {
                    goal_id: id.0.clone(),
                    goal_name: goal.name.clone(),
                    importance: goal.importance,
                    factor,
                }),
            )
        }
    }
}

/// Drift Project fields toward baseline over event-log time. Triggered by
/// the "freshest" Event in the log (read via `Now`), not the wall clock —
/// so replay is deterministic (ADR-0004).
///
/// Formula: `field = baseline + (field - baseline) * factor.powf(days)`.
/// `factor = 0.95` per day → roughly 5% closer to baseline per quiet day.
///
/// Archived and Completed projects are skipped — they no longer pull on
/// the user's attention.
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
        if project.status != ProjectStatus::Active {
            continue;
        }
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

use chrono::{DateTime, Utc};
use liferuntime_event_log::EventId;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

use crate::model::ProjectStatus;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub tags: Vec<String>,
    /// Raw `strategic_relevance` — mutated only by matching and decay.
    /// Decisions never touch this field; their boost is an additive
    /// layer surfaced via [`Self::strategic_relevance_visible`] per
    /// ADR-0008 `#chosen-decaying-boost-not-a-floor`.
    pub strategic_relevance_raw: f32,
    /// User-facing `strategic_relevance` — `raw + active Decision boost`,
    /// clamped to `[0.0, 1.0]`. Equal to `_raw` when no Decision
    /// steers this project.
    pub strategic_relevance_visible: f32,
    pub urgency: f32,
    pub status: ProjectStatus,
    pub archived_reason: Option<String>,
    pub completion_note: Option<String>,
    /// Light declarative annotation (CONTEXT.md `#depends_on`). Renderers
    /// surface the list verbatim — no traversal, no system effects.
    /// Cycles are permitted; the field is always rendered flat.
    pub depends_on: Vec<String>,
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

/// One row in the polished `liferuntime decision list` output (issue #7).
///
/// The shape preserves both the **original** event payload (`chose`,
/// `over`, `dampen`, `decided_at`) and the **derived** current state
/// (`steers`, `superseded_for`). Renderers compose the two to show the
/// full lifecycle of a Decision: what the user committed to, and what
/// it still controls after later Decisions / revocations.
#[derive(Clone, Debug)]
pub struct DecisionListView {
    pub decision_id: EventId,
    pub chose: String,
    pub over: Vec<String>,
    pub dampen: Vec<String>,
    /// Effective decision date — the event's `decided_at` if set,
    /// otherwise the envelope's `occurred_at`.
    pub decided_at: DateTime<Utc>,
    /// Integer number of full event-log days between `decided_at` and
    /// the current event-log time (ADR-0004).
    pub active_event_log_days: i64,
    /// Projects this Decision still steers. Order: chose-projects
    /// first, then dampened (per issue #7 spec).
    pub steers: Vec<DecisionSteerView>,
    /// Projects this Decision originally targeted but a later Decision
    /// has since taken over. Order: insertion order of the originating
    /// (chose, then dampen) lists.
    pub superseded_for: Vec<DecisionSupersessionView>,
}

#[derive(Clone, Debug)]
pub struct DecisionSteerView {
    pub project_id: String,
    pub kind: SteerKind,
    /// `Some(remaining)` only for `Chose` stances; `None` for
    /// `Dampened` (no boost component exists for dampened projects).
    pub boost_remaining: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteerKind {
    Chose,
    Dampened,
}

#[derive(Clone, Debug)]
pub struct DecisionSupersessionView {
    pub project_id: String,
    pub by_decision_id: EventId,
}

/// Render a list of [`DecisionListView`]s to the format mandated by
/// issue #7. The format is pre-specified so a CLI-side test can verify
/// it byte-for-byte.
///
/// ```text
/// <decision_id> chose:<project_id> over:[<id>,<id>,...] dampen:[<id>,<id>,...]
///   decided <YYYY-MM-DD>, active <N> event-log days
///   steers <project_id> (chose, boost <X.XXX> remaining of 0.150)
///   steers <project_id> (dampened)
///   superseded for <project_id> by <decision_id>
/// ```
///
/// Decision blocks are separated by a single blank line; the rendered
/// string always ends with a trailing newline so callers can `print!`
/// it directly.
pub fn format_decision_list(views: &[DecisionListView]) -> String {
    let mut out = String::new();
    for (i, v) in views.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        writeln!(
            out,
            "{} chose:{} over:[{}] dampen:[{}]",
            v.decision_id,
            v.chose,
            v.over.join(","),
            v.dampen.join(",")
        )
        .expect("string write");
        writeln!(
            out,
            "  decided {}, active {} event-log days",
            v.decided_at.format("%Y-%m-%d"),
            v.active_event_log_days,
        )
        .expect("string write");
        for steer in &v.steers {
            match steer.kind {
                SteerKind::Chose => {
                    let remaining = steer.boost_remaining.unwrap_or(0.0);
                    writeln!(
                        out,
                        "  steers {} (chose, boost {:.3} remaining of 0.150)",
                        steer.project_id, remaining,
                    )
                    .expect("string write");
                }
                SteerKind::Dampened => {
                    writeln!(out, "  steers {} (dampened)", steer.project_id)
                        .expect("string write");
                }
            }
        }
        for s in &v.superseded_for {
            writeln!(
                out,
                "  superseded for {} by {}",
                s.project_id, s.by_decision_id
            )
            .expect("string write");
        }
    }
    out
}

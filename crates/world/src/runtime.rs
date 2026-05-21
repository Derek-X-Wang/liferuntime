use bevy_ecs::prelude::*;
use chrono::{DateTime, Utc};
use liferuntime_event_log::{
    EventId, EventLog, EventRange, JsonlEventLog, MemoryEventLog, StoredEvent,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};

use crate::errors::RuntimeError;
use crate::events::WorldEvent;
use crate::explanation::{ChangeLog, ChangeRecord, ExplainTarget, Explanation};
use crate::model::{
    DecisionBoost, DecisionStance, Goal, GoalStatus, Identity, LastTouched, LatestEventId, Now,
    PendingDecision, Project, ProjectStatus, RecordedDecisions, Signal, Unprocessed,
};
use crate::queries::{
    DecisionListView, DecisionSteerView, DecisionSupersessionView, ProjectTrajectoryView,
    ProjectView, SteerKind,
};
use crate::systems::register_systems;

/// Persistent cursor for "what has the user already advanced through".
///
/// Stored next to `events.jsonl` as `cursor.json`. Without this, every
/// advance after process restart would re-report all derivations from the
/// entire history.
#[derive(Default, Clone, Serialize, Deserialize)]
struct Cursor {
    last_event_id: Option<EventId>,
}

/// One row in `advances.jsonl` — appended after every successful Advance.
/// Lays the foundation for Trajectory (slope) queries: K most-recent rows
/// describe the recent direction of a Project's fields.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdvanceRecord {
    pub advanced_at: DateTime<Utc>,
    pub cursor_at_advance: Option<EventId>,
    pub records: Vec<ChangeRecord>,
}

/// The deterministic core of LifeRuntime.
pub struct WorldRuntime {
    world: World,
    schedule: Schedule,
    log: EventLogBackend,
    cursor: Cursor,
    dir: Option<PathBuf>,
    /// Exclusive flock on `.liferuntime/lock`. Held for the lifetime of
    /// the runtime; serializes concurrent CLI processes hitting the
    /// same dir so cursor / events.jsonl don't race. Released on drop
    /// (file close releases the OS lock).
    _lock: Option<File>,
    /// Idempotency keys we have already accepted (rebuilt from the log
    /// on `open_dir`). A second ingest with the same key returns the
    /// existing event id and does nothing else.
    seen_keys: HashMap<String, EventId>,
}

enum EventLogBackend {
    Memory(MemoryEventLog<WorldEvent>),
    Jsonl(JsonlEventLog<WorldEvent>),
}

impl EventLogBackend {
    fn append(&mut self, payload: WorldEvent) -> Result<StoredEvent<WorldEvent>, RuntimeError> {
        let stored = StoredEvent::new(payload);
        self.append_stored(stored)
    }

    fn append_stored(
        &mut self,
        stored: StoredEvent<WorldEvent>,
    ) -> Result<StoredEvent<WorldEvent>, RuntimeError> {
        match self {
            Self::Memory(l) => {
                l.append(stored.clone())
                    .unwrap_or_else(|never| match never {});
            }
            Self::Jsonl(l) => {
                l.append(stored.clone())?;
            }
        }
        Ok(stored)
    }

    fn replay_all(&self) -> Result<Vec<StoredEvent<WorldEvent>>, RuntimeError> {
        match self {
            Self::Memory(l) => Ok(l
                .replay(EventRange::All)
                .unwrap_or_else(|never| match never {})),
            Self::Jsonl(l) => Ok(l.replay(EventRange::All)?),
        }
    }
}

#[derive(Clone, Debug)]
pub struct IngestReceipt {
    pub event_id: EventId,
}

#[derive(Clone, Debug)]
pub struct WorldChanges {
    pub records: Vec<ChangeRecord>,
}

impl WorldChanges {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn contains_change_for(&self, entity_id: &str) -> bool {
        self.records.iter().any(|r| r.entity_id == entity_id)
    }
}

impl WorldRuntime {
    pub fn in_memory() -> Result<Self, RuntimeError> {
        Self::with_backend(EventLogBackend::Memory(MemoryEventLog::default()), None)
    }

    pub fn open_dir(dir: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;

        // Acquire the dir-level exclusive lock BEFORE touching any
        // other file in the dir. Blocks if another process holds it.
        let lock_path = dir.join("lock");
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        lock_file.lock()?;

        let log = JsonlEventLog::open(dir.join("events.jsonl"))?;
        let cursor = load_cursor(&dir.join("cursor.json"))?;
        let mut rt = Self::with_backend(EventLogBackend::Jsonl(log), Some(dir))?;
        rt.cursor = cursor;
        rt._lock = Some(lock_file);
        rt.replay()?;
        Ok(rt)
    }

    fn with_backend(log: EventLogBackend, dir: Option<PathBuf>) -> Result<Self, RuntimeError> {
        let mut world = World::new();
        world.init_resource::<ChangeLog>();
        world.init_resource::<Now>();
        world.init_resource::<LatestEventId>();
        world.init_resource::<RecordedDecisions>();

        let mut schedule = Schedule::default();
        register_systems(&mut schedule);

        Ok(Self {
            world,
            schedule,
            log,
            cursor: Cursor::default(),
            dir,
            _lock: None,
            seen_keys: HashMap::new(),
        })
    }

    fn replay(&mut self) -> Result<(), RuntimeError> {
        let events = self.log.replay_all()?;
        for stored in events {
            if let Some(key) = &stored.idempotency_key {
                self.seen_keys.insert(key.clone(), stored.id.clone());
            }
            self.apply_and_derive(&stored);
        }
        Ok(())
    }

    pub fn ingest(&mut self, event: WorldEvent) -> Result<IngestReceipt, RuntimeError> {
        self.ingest_with_key(event, None)
    }

    /// Ingest an event with an optional idempotency key. A second
    /// ingest carrying the same key (within the same log) is a no-op
    /// that returns the original event's id.
    pub fn ingest_with_key(
        &mut self,
        event: WorldEvent,
        key: Option<String>,
    ) -> Result<IngestReceipt, RuntimeError> {
        self.validate_event(&event)?;
        if let Some(k) = &key {
            if let Some(existing) = self.seen_keys.get(k) {
                return Ok(IngestReceipt {
                    event_id: existing.clone(),
                });
            }
        }
        let stored = match key.clone() {
            Some(k) => self
                .log
                .append_stored(StoredEvent::with_idempotency_key(event, k))?,
            None => self.log.append(event)?,
        };
        if let Some(k) = key {
            self.seen_keys.insert(k, stored.id.clone());
        }
        self.apply_and_derive(&stored);
        Ok(IngestReceipt {
            event_id: stored.id,
        })
    }

    /// Reject events that would violate runtime invariants: duplicate
    /// entity creation, edits / lifecycle transitions on non-existent
    /// entities, out-of-range numeric fields. Called at ingest time so
    /// the log never contains invalid events.
    fn validate_event(&mut self, event: &WorldEvent) -> Result<(), RuntimeError> {
        match event {
            WorldEvent::ProjectCreated { id, .. } => {
                if self.project_exists(id) {
                    return Err(RuntimeError::DuplicateEntity {
                        kind: "Project",
                        id: id.clone(),
                    });
                }
            }
            WorldEvent::GoalCreated { id, importance, .. } => {
                if self.goal_exists(id) {
                    return Err(RuntimeError::DuplicateEntity {
                        kind: "Goal",
                        id: id.clone(),
                    });
                }
                check_unit("importance", *importance)?;
            }
            WorldEvent::ProjectUpdated { id, depends_on, .. } => {
                if !self.project_exists(id) {
                    return Err(RuntimeError::EntityNotFound {
                        kind: "Project",
                        id: id.clone(),
                    });
                }
                // CONTEXT.md `#depends_on`: every referenced id must
                // resolve to an existing Project. Cycles are permitted
                // (the list is a declarative annotation, not a
                // traversable graph), so no graph validation here.
                if let Some(deps) = depends_on {
                    for dep_id in deps {
                        if !self.project_exists(dep_id) {
                            return Err(RuntimeError::EntityNotFound {
                                kind: "Project",
                                id: dep_id.clone(),
                            });
                        }
                    }
                }
            }
            WorldEvent::GoalUpdated { id, importance, .. } => {
                if !self.goal_exists(id) {
                    return Err(RuntimeError::EntityNotFound {
                        kind: "Goal",
                        id: id.clone(),
                    });
                }
                if let Some(i) = importance {
                    check_unit("importance", *i)?;
                }
            }
            WorldEvent::SignalObserved { confidence, .. } => {
                check_unit("confidence", *confidence)?;
            }
            WorldEvent::ProjectArchived { id, .. }
            | WorldEvent::ProjectCompleted { id, .. }
            | WorldEvent::ProjectReactivated { id } => {
                if !self.project_exists(id) {
                    return Err(RuntimeError::EntityNotFound {
                        kind: "Project",
                        id: id.clone(),
                    });
                }
            }
            WorldEvent::GoalAchieved { id, .. }
            | WorldEvent::GoalAbandoned { id, .. }
            | WorldEvent::GoalReactivated { id } => {
                if !self.goal_exists(id) {
                    return Err(RuntimeError::EntityNotFound {
                        kind: "Goal",
                        id: id.clone(),
                    });
                }
            }
            WorldEvent::TimePulseObserved { .. } => { /* no validation */ }
            WorldEvent::DecisionRecorded {
                chose,
                over,
                dampen,
                ..
            } => {
                // ADR-0008 + issue #1: every project id referenced by a
                // Decision must already exist. Reject otherwise so the
                // log never contains a Decision pointing at a ghost
                // project — keeps stance derivation (issue #2) simple
                // and replay-safe.
                if !self.project_exists(chose) {
                    return Err(RuntimeError::EntityNotFound {
                        kind: "Project",
                        id: chose.clone(),
                    });
                }
                for id in over.iter().chain(dampen.iter()) {
                    if !self.project_exists(id) {
                        return Err(RuntimeError::EntityNotFound {
                            kind: "Project",
                            id: id.clone(),
                        });
                    }
                }
            }
            WorldEvent::DecisionRevoked { decision_id } => {
                // ADR-0008 `#lifecycle` (amended): ingest rejects
                // unknown ids loudly; replay silently ignores them.
                // The asymmetry lives here — `validate_event` runs at
                // ingest, never during replay.
                let recorded = self.world.resource::<RecordedDecisions>();
                if !recorded.contains(decision_id) {
                    return Err(RuntimeError::EntityNotFound {
                        kind: "Decision",
                        id: decision_id.0.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    fn project_exists(&mut self, id: &str) -> bool {
        let mut q = self.world.query::<(&Identity, &Project)>();
        q.iter(&self.world).any(|(ident, _)| ident.0 == id)
    }

    fn goal_exists(&mut self, id: &str) -> bool {
        let mut q = self.world.query::<(&Identity, &Goal)>();
        q.iter(&self.world).any(|(ident, _)| ident.0 == id)
    }

    /// Test / fixture helper: ingest an event with an explicit timestamp.
    /// Production callers should use [`Self::ingest`].
    pub fn ingest_at(
        &mut self,
        event: WorldEvent,
        at: DateTime<Utc>,
    ) -> Result<IngestReceipt, RuntimeError> {
        let stored = StoredEvent {
            id: EventId::new(),
            occurred_at: at,
            idempotency_key: None,
            payload: event,
        };
        let stored = self.log.append_stored(stored)?;
        self.apply_and_derive(&stored);
        Ok(IngestReceipt {
            event_id: stored.id,
        })
    }

    /// Apply an event AND run the schedule against the resulting world
    /// state. This is the per-event scheduling model (ADR-0006): every
    /// event derives state immediately, so each event "sees" only the
    /// past. Eliminates a class of temporal-coupling bugs at the cost of
    /// O(N events × schedule) replay.
    fn apply_and_derive(&mut self, stored: &StoredEvent<WorldEvent>) {
        apply_event(&mut self.world, stored);
        self.schedule.run(&mut self.world);
    }

    /// Report the changes triggered by events past the cursor, advance
    /// the cursor, persist it. Does **not** run systems — under ADR-0006
    /// systems already ran at ingest time. Advance is purely a reader.
    pub fn advance(&mut self) -> Result<WorldChanges, RuntimeError> {
        let cursor_id = self.cursor.last_event_id.clone();
        let records = self.world.resource::<ChangeLog>().records.clone();
        let new_records: Vec<ChangeRecord> = records
            .into_iter()
            .filter(|r| cursor_id.as_ref().is_none_or(|c| r.triggered_by_event > *c))
            .collect();

        if let Some(latest) = self.log.replay_all()?.iter().map(|e| e.id.clone()).max() {
            self.cursor.last_event_id = Some(latest);
            if let Some(dir) = &self.dir {
                save_cursor(&dir.join("cursor.json"), &self.cursor)?;
            }
        }

        if let Some(dir) = &self.dir {
            append_advance(
                &dir.join("advances.jsonl"),
                &AdvanceRecord {
                    advanced_at: Utc::now(),
                    cursor_at_advance: self.cursor.last_event_id.clone(),
                    records: new_records.clone(),
                },
            )?;
        }

        Ok(WorldChanges {
            records: new_records,
        })
    }

    /// Current per-project Decision stance, keyed by project id. Built
    /// by replay of `DecisionRecorded` events via the
    /// `decision_application_system` (ADR-0008
    /// `#per-project-stance-derived-by-replay`).
    ///
    /// **Source of truth for all Decision-driven mechanics.** The
    /// decaying-boost system (issue #4), resonance dampening (issue
    /// #5), and the `decision list` CLI (issue #7) all consume *this*
    /// API rather than walking the event log themselves — there must
    /// be one derivation of stance, not several.
    pub fn decision_stances(&mut self) -> HashMap<String, DecisionStance> {
        let mut q = self.world.query::<(&Identity, &DecisionStance)>();
        q.iter(&self.world)
            .map(|(ident, stance)| (ident.0.clone(), stance.clone()))
            .collect()
    }

    /// Build the polished view consumed by `liferuntime decision list`
    /// (issue #7).
    ///
    /// Walks the event log in insertion order to recover each
    /// Decision's *original* payload (chose / over / dampen /
    /// decided_at), then joins against the derived current stance +
    /// boost components to compute `steers` and `superseded_for`.
    /// Revoked Decisions are excluded.
    ///
    /// Returns entries in chronological (replay-order) order.
    pub fn decision_list(&mut self) -> Result<Vec<DecisionListView>, RuntimeError> {
        // 1. Walk the log: collect Decision metadata + revoke set.
        let events = self.log.replay_all()?;
        let mut revoked: std::collections::HashSet<EventId> = std::collections::HashSet::new();
        struct Meta {
            decision_id: EventId,
            chose: String,
            over: Vec<String>,
            dampen: Vec<String>,
            decided_at: DateTime<Utc>,
        }
        let mut metas: Vec<Meta> = Vec::new();
        for stored in &events {
            match &stored.payload {
                WorldEvent::DecisionRecorded {
                    chose,
                    over,
                    dampen,
                    decided_at,
                    ..
                } => {
                    metas.push(Meta {
                        decision_id: stored.id.clone(),
                        chose: chose.clone(),
                        over: over.clone(),
                        dampen: dampen.clone(),
                        decided_at: decided_at.unwrap_or(stored.occurred_at),
                    });
                }
                WorldEvent::DecisionRevoked { decision_id } => {
                    revoked.insert(decision_id.clone());
                }
                _ => {}
            }
        }

        // 2. Snapshot current per-project stance + boost.
        let mut current: HashMap<String, (DecisionStance, Option<f32>)> = HashMap::new();
        let mut q = self
            .world
            .query::<(&Identity, &DecisionStance, Option<&DecisionBoost>)>();
        for (ident, stance, boost) in q.iter(&self.world) {
            current.insert(
                ident.0.clone(),
                (stance.clone(), boost.map(|b| b.remaining)),
            );
        }
        let now = self.world.resource::<Now>().at();

        // 3. Compose views.
        let mut out: Vec<DecisionListView> = Vec::new();
        for meta in metas {
            if revoked.contains(&meta.decision_id) {
                continue;
            }

            let mut steers: Vec<DecisionSteerView> = Vec::new();
            let mut superseded: Vec<DecisionSupersessionView> = Vec::new();

            let mut classify = |proj_id: &String, kind: SteerKind| {
                if let Some((stance, boost)) = current.get(proj_id) {
                    if stance.decision_id() == &meta.decision_id {
                        steers.push(DecisionSteerView {
                            project_id: proj_id.clone(),
                            kind,
                            boost_remaining: *boost,
                        });
                    } else {
                        superseded.push(DecisionSupersessionView {
                            project_id: proj_id.clone(),
                            by_decision_id: stance.decision_id().clone(),
                        });
                    }
                }
                // No current stance → silently omit. The Decision
                // originally targeted this project, but a later
                // Decision claimed it and was then revoked (or some
                // other clearing path); there's nothing to "supersede
                // for" if the new owner is gone.
            };

            // chose first.
            classify(&meta.chose, SteerKind::Chose);
            // dampened, skipping any project also named in chose to
            // avoid double-counting (chose wins).
            for proj_id in &meta.dampen {
                if proj_id == &meta.chose {
                    continue;
                }
                classify(proj_id, SteerKind::Dampened);
            }

            let active_days = (now - meta.decided_at).num_days().max(0);

            out.push(DecisionListView {
                decision_id: meta.decision_id,
                chose: meta.chose,
                over: meta.over,
                dampen: meta.dampen,
                decided_at: meta.decided_at,
                active_event_log_days: active_days,
                steers,
                superseded_for: superseded,
            });
        }
        Ok(out)
    }

    pub fn inspect_project(&mut self, id: &str) -> Option<ProjectView> {
        let mut q = self
            .world
            .query::<(&Identity, &Project, Option<&DecisionBoost>)>();
        for (ident, project, boost) in q.iter(&self.world) {
            if ident.0 == id {
                let raw = project.strategic_relevance;
                let boost_contrib = boost.map(|b| b.remaining).unwrap_or(0.0);
                let visible = (raw + boost_contrib).clamp(0.0, 1.0);
                return Some(ProjectView {
                    id: ident.0.clone(),
                    name: project.name.clone(),
                    tags: project.tags.clone(),
                    strategic_relevance_raw: raw,
                    strategic_relevance_visible: visible,
                    urgency: project.urgency,
                    status: project.status,
                    archived_reason: project.archived_reason.clone(),
                    completion_note: project.completion_note.clone(),
                    depends_on: project.depends_on.clone(),
                });
            }
        }
        None
    }

    pub fn explain(&self, target: ExplainTarget) -> Result<Explanation, RuntimeError> {
        let records = self.world.resource::<ChangeLog>().records.clone();
        let scoped: Vec<ChangeRecord> = match target {
            ExplainTarget::LatestChange => records,
            ExplainTarget::Entity(id) => {
                records.into_iter().filter(|r| r.entity_id == id).collect()
            }
        };
        Ok(Explanation { records: scoped })
    }

    /// Full causal history for a Project (CONTEXT.md `#explanation`,
    /// issue #16): every `ChangeRecord` whose `entity_id` matches the
    /// target, sorted by triggering event id (event-log order).
    ///
    /// ChangeRecord-only — the `ProjectCreated` event is **not** part
    /// of the chain (read the event log directly for creation context).
    /// An untouched project returns `Ok(Explanation { records: vec![] })`;
    /// callers render the friendly "no changes yet" state. Unknown ids
    /// return `Err(EntityNotFound)` so the CLI can exit non-zero.
    pub fn explain_project_history(&mut self, id: &str) -> Result<Explanation, RuntimeError> {
        if !self.project_exists(id) {
            return Err(RuntimeError::EntityNotFound {
                kind: "Project",
                id: id.to_string(),
            });
        }
        let records = self.world.resource::<ChangeLog>().records.clone();
        let mut filtered: Vec<ChangeRecord> =
            records.into_iter().filter(|r| r.entity_id == id).collect();
        // Insertion order is already event-log order under per-event
        // scheduling (ADR-0006), but pin the contract with an explicit
        // stable sort so callers can rely on it independently of the
        // schedule shape. `sort_by` is stable, preserving the relative
        // order of records emitted by the same event (e.g. matching's
        // strategic_relevance + urgency pair).
        filtered.sort_by(|a, b| a.triggered_by_event.cmp(&b.triggered_by_event));
        Ok(Explanation { records: filtered })
    }

    pub fn event_count(&self) -> usize {
        self.log.replay_all().map(|v| v.len()).unwrap_or(0)
    }

    /// Number of Events past the current advance cursor. When > 0, the
    /// last persisted `advance` output (in `last_advance.json`) is stale
    /// — there are derivations the user has not yet acknowledged.
    pub fn pending_events(&self) -> Result<usize, RuntimeError> {
        let cursor = self.cursor.last_event_id.clone();
        let events = self.log.replay_all()?;
        let n = match cursor {
            None => events.len(),
            Some(c) => events.iter().filter(|e| e.id > c).count(),
        };
        Ok(n)
    }

    /// Read the last `window` rows of `advances.jsonl` and compute, for
    /// each Project, the average per-advance change in
    /// `strategic_relevance`. Use this as the Trajectory slope.
    ///
    /// Projects with no recorded change in the window get a 0.0 slope
    /// and `advances_observed: 0`. Useful for `liferuntime status`.
    pub fn trajectories(
        &mut self,
        window: usize,
    ) -> Result<Vec<ProjectTrajectoryView>, RuntimeError> {
        let advances = self.recent_advances(window)?;

        // For each entity: how many advances in the window touched it,
        // and what was the net delta in the most recent of those.
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut last_delta: HashMap<String, f32> = HashMap::new();
        for adv in &advances {
            let mut per_entity_this_advance: HashMap<String, f32> = HashMap::new();
            for r in &adv.records {
                if r.field == "strategic_relevance" {
                    *per_entity_this_advance
                        .entry(r.entity_id.clone())
                        .or_insert(0.0) += r.after - r.before;
                }
            }
            for (entity, delta) in per_entity_this_advance {
                *counts.entry(entity.clone()).or_insert(0) += 1;
                // Iteration is oldest → newest, so overwriting yields the latest.
                last_delta.insert(entity, delta);
            }
        }

        let mut q = self.world.query::<(&Identity, &Project)>();
        let mut out: Vec<ProjectTrajectoryView> = Vec::new();
        for (ident, project) in q.iter(&self.world) {
            let slope = last_delta.get(&ident.0).copied().unwrap_or(0.0);
            let count = counts.get(&ident.0).copied().unwrap_or(0);
            out.push(ProjectTrajectoryView {
                id: ident.0.clone(),
                name: project.name.clone(),
                status: project.status,
                current_relevance: project.strategic_relevance,
                current_urgency: project.urgency,
                slope_relevance: slope,
                advances_observed: count,
            });
        }
        Ok(out)
    }

    fn recent_advances(&self, window: usize) -> Result<Vec<AdvanceRecord>, RuntimeError> {
        let Some(dir) = &self.dir else {
            return Ok(Vec::new());
        };
        let path = dir.join("advances.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&path)?;
        let mut all: Vec<AdvanceRecord> = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let rec: AdvanceRecord = serde_json::from_str(&line)?;
            all.push(rec);
        }
        let total = all.len();
        Ok(all.into_iter().skip(total.saturating_sub(window)).collect())
    }
}

fn check_unit(field: &'static str, value: f32) -> Result<(), RuntimeError> {
    if !(0.0..=1.0).contains(&value) || value.is_nan() {
        return Err(RuntimeError::ValueOutOfRange {
            field,
            value,
            min: 0.0,
            max: 1.0,
        });
    }
    Ok(())
}

fn apply_event(world: &mut World, stored: &StoredEvent<WorldEvent>) {
    // Maintain time-tracking resources first so any system reading them
    // sees the new event's clock position.
    //
    // TimePulseObserved is special: its *payload* observed_at is the
    // intended clock position (often "fast-forward N days"), while the
    // envelope occurred_at is just ingest wall-clock. Use the payload
    // for pulses so decay actually catches up.
    let effective_now = match &stored.payload {
        WorldEvent::TimePulseObserved { observed_at } => *observed_at,
        _ => stored.occurred_at,
    };
    {
        let mut now = world.resource_mut::<Now>();
        if effective_now > now.at() {
            *now = Now(effective_now);
        }
    }
    {
        let mut latest = world.resource_mut::<LatestEventId>();
        if latest.get().is_none_or(|prev| stored.id > *prev) {
            *latest = LatestEventId(Some(stored.id.clone()));
        }
    }

    match &stored.payload {
        WorldEvent::ProjectCreated { id, name, tags } => {
            // Idempotent at the apply step: validate_event already rejected
            // duplicates at ingest, but a hand-corrupted log shouldn't
            // produce duplicate entities on replay.
            let mut q = world.query::<&Identity>();
            if q.iter(world).any(|i| i.0 == *id) {
                return;
            }
            world.spawn((
                Identity(id.clone()),
                Project::new(name.clone(), tags.clone()),
                LastTouched {
                    at: stored.occurred_at,
                },
            ));
        }
        WorldEvent::ProjectUpdated {
            id,
            name,
            tags,
            depends_on,
        } => {
            let mut q = world.query::<(&Identity, &mut Project)>();
            for (ident, mut project) in q.iter_mut(world) {
                if ident.0 == *id {
                    if let Some(n) = name {
                        project.name = n.clone();
                    }
                    if let Some(t) = tags {
                        project.tags = t.clone();
                    }
                    // `Some(_)` is full-replace (mirrors `tags`); `None`
                    // leaves the field untouched. `Some(empty)` clears
                    // the list — the only path to an empty annotation
                    // after a prior non-empty update.
                    if let Some(d) = depends_on {
                        project.depends_on = d.clone();
                    }
                    break;
                }
            }
        }
        WorldEvent::GoalCreated {
            id,
            name,
            tags,
            importance,
        } => {
            let mut q = world.query::<&Identity>();
            if q.iter(world).any(|i| i.0 == *id) {
                return;
            }
            world.spawn((
                Identity(id.clone()),
                Goal::new(name.clone(), tags.clone(), *importance),
            ));
        }
        WorldEvent::GoalUpdated {
            id,
            name,
            tags,
            importance,
        } => {
            let mut q = world.query::<(&Identity, &mut Goal)>();
            for (ident, mut goal) in q.iter_mut(world) {
                if ident.0 == *id {
                    if let Some(n) = name {
                        goal.name = n.clone();
                    }
                    if let Some(t) = tags {
                        goal.tags = t.clone();
                    }
                    if let Some(i) = importance {
                        goal.importance = *i;
                    }
                    break;
                }
            }
        }
        WorldEvent::SignalObserved {
            source,
            summary,
            tags,
            confidence,
            observed_at,
        } => {
            world.spawn((
                Signal {
                    triggering_event_id: stored.id.clone(),
                    source: source.clone(),
                    summary: summary.clone(),
                    tags: tags.clone(),
                    confidence: *confidence,
                    observed_at: observed_at.unwrap_or(stored.occurred_at),
                },
                Unprocessed,
            ));
        }
        WorldEvent::ProjectArchived { id, reason } => {
            let mut q = world.query::<(&Identity, &mut Project)>();
            for (ident, mut project) in q.iter_mut(world) {
                if ident.0 == *id {
                    project.status = ProjectStatus::Archived;
                    project.archived_reason = reason.clone();
                    break;
                }
            }
        }
        WorldEvent::ProjectCompleted { id, note } => {
            let mut q = world.query::<(&Identity, &mut Project)>();
            for (ident, mut project) in q.iter_mut(world) {
                if ident.0 == *id {
                    project.status = ProjectStatus::Completed;
                    project.completion_note = note.clone();
                    break;
                }
            }
        }
        WorldEvent::ProjectReactivated { id } => {
            let mut q = world.query::<(&Identity, &mut Project)>();
            for (ident, mut project) in q.iter_mut(world) {
                if ident.0 == *id {
                    project.status = ProjectStatus::Active;
                    project.archived_reason = None;
                    project.completion_note = None;
                    break;
                }
            }
        }
        WorldEvent::TimePulseObserved { .. } => {
            // No entity to spawn. The Now / LatestEventId resource
            // updates above already advanced event-log time, which is
            // the pulse's entire purpose.
        }
        WorldEvent::GoalAchieved { id, note } => {
            let mut q = world.query::<(&Identity, &mut Goal)>();
            for (ident, mut goal) in q.iter_mut(world) {
                if ident.0 == *id {
                    goal.status = GoalStatus::Achieved;
                    goal.achievement_note = note.clone();
                    break;
                }
            }
        }
        WorldEvent::GoalAbandoned { id, reason } => {
            let mut q = world.query::<(&Identity, &mut Goal)>();
            for (ident, mut goal) in q.iter_mut(world) {
                if ident.0 == *id {
                    goal.status = GoalStatus::Abandoned;
                    goal.abandonment_reason = reason.clone();
                    break;
                }
            }
        }
        WorldEvent::GoalReactivated { id } => {
            let mut q = world.query::<(&Identity, &mut Goal)>();
            for (ident, mut goal) in q.iter_mut(world) {
                if ident.0 == *id {
                    goal.status = GoalStatus::Active;
                    goal.achievement_note = None;
                    goal.abandonment_reason = None;
                    break;
                }
            }
        }
        WorldEvent::DecisionRecorded { chose, dampen, .. } => {
            // Issue #2: spawn a PendingDecision so the
            // `decision_application_system` (running on the per-event
            // schedule from ADR-0006) flips targeted project stances
            // before any later system observes them. The transient
            // entity is despawned by the system after it applies the
            // stance changes.
            //
            // `over` is deliberately not carried into the ECS — per
            // ADR-0008 it has no mechanical effect; consumers that
            // want narrative rivals (e.g. `decision list` polish in
            // issue #7) read the original event from the log by
            // decision_id.
            world.spawn(PendingDecision {
                decision_id: stored.id.clone(),
                chose: chose.clone(),
                dampen: dampen.clone(),
            });
            // Remember this decision_id so future DecisionRevoked
            // events can validate at ingest and silently no-op at
            // replay (issue #3).
            world
                .resource_mut::<RecordedDecisions>()
                .insert(stored.id.clone());
        }
        WorldEvent::DecisionRevoked { decision_id } => {
            // Replay tolerance (ADR-0008 `#lifecycle` amendment):
            // silently ignore a revoke whose target was never
            // recorded. At ingest, `validate_event` already rejected
            // this payload — only a hand-corrupted log can reach this
            // branch with an unknown id.
            let known = world.resource::<RecordedDecisions>().contains(decision_id);
            if !known {
                return;
            }

            // Clear every project whose current stance is owned by the
            // revoked Decision. Projects that were originally steered
            // by this Decision but have since been superseded by a
            // later one are unaffected — their stance is owned by the
            // later Decision.
            let mut q = world.query::<(Entity, &DecisionStance)>();
            let to_clear: Vec<Entity> = q
                .iter(world)
                .filter_map(|(entity, stance)| {
                    if stance.decision_id() == decision_id {
                        Some(entity)
                    } else {
                        None
                    }
                })
                .collect();
            for entity in to_clear {
                let mut em = world.entity_mut(entity);
                em.remove::<DecisionStance>();
                em.remove::<DecisionBoost>();
            }
        }
    }
}

fn load_cursor(path: &Path) -> Result<Cursor, RuntimeError> {
    if !path.exists() {
        return Ok(Cursor::default());
    }
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn save_cursor(path: &Path, cursor: &Cursor) -> Result<(), RuntimeError> {
    let bytes = serde_json::to_vec_pretty(cursor)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

fn append_advance(path: &Path, record: &AdvanceRecord) -> Result<(), RuntimeError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(record)?;
    writeln!(file, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::WorldEvent;
    use chrono::{Duration, TimeZone, Utc};

    fn project(id: &str, tags: &[&str]) -> WorldEvent {
        WorldEvent::ProjectCreated {
            id: id.into(),
            name: id.to_uppercase(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn signal(summary: &str, tags: &[&str], confidence: f32) -> WorldEvent {
        WorldEvent::SignalObserved {
            source: "test".into(),
            summary: summary.into(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            confidence,
            observed_at: None,
        }
    }

    #[test]
    fn signal_about_realtime_voice_increases_tnt_relevance_with_explanation() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        runtime
            .ingest(project("tnt", &["ai", "voice", "agent"]))
            .unwrap();
        runtime
            .ingest(signal(
                "Realtime voice models are improving quickly",
                &["ai", "voice", "realtime"],
                0.8,
            ))
            .unwrap();
        let changes = runtime.advance().unwrap();
        assert!(
            changes.contains_change_for("tnt"),
            "expected tnt to receive a change, got {:?}",
            changes.records
        );

        let strategic = changes
            .records
            .iter()
            .find(|r| r.entity_id == "tnt" && r.field == "strategic_relevance")
            .expect("strategic_relevance change");
        assert!(strategic.after > strategic.before);

        let explanation = runtime.explain(ExplainTarget::LatestChange).unwrap();
        let rendered = explanation.to_string();
        assert!(
            rendered.contains("tnt"),
            "explanation missing entity: {rendered}"
        );
        assert!(
            rendered.contains("voice"),
            "explanation missing tag: {rendered}"
        );
        assert!(
            rendered.contains("Realtime voice"),
            "explanation missing signal summary: {rendered}"
        );
    }

    #[test]
    fn replay_is_deterministic_for_same_inputs() {
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let events: Vec<(WorldEvent, _)> = vec![
            (project("p1", &["ai", "voice"]), t0),
            (
                signal("voice models improving", &["ai", "voice"], 0.7),
                t0 + Duration::hours(1),
            ),
            (
                signal("ai funding wave", &["ai"], 0.5),
                t0 + Duration::hours(2),
            ),
        ];

        let mut a = WorldRuntime::in_memory().unwrap();
        let mut b = WorldRuntime::in_memory().unwrap();
        for (e, at) in &events {
            a.ingest_at(e.clone(), *at).unwrap();
            b.ingest_at(e.clone(), *at).unwrap();
        }
        let ca = a.advance().unwrap();
        let cb = b.advance().unwrap();
        assert_eq!(
            ca.records.len(),
            cb.records.len(),
            "record counts differ: {:?} vs {:?}",
            ca.records,
            cb.records
        );
        for (x, y) in ca.records.iter().zip(cb.records.iter()) {
            assert_eq!(x.entity_id, y.entity_id);
            assert_eq!(x.field, y.field);
            assert!((x.before - y.before).abs() < 1e-6);
            assert!((x.after - y.after).abs() < 1e-6);
        }
    }

    #[test]
    fn unmatched_signal_changes_nothing() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        runtime.ingest(project("tnt", &["ai", "voice"])).unwrap();
        runtime
            .ingest(signal("random news", &["politics"], 0.9))
            .unwrap();
        let changes = runtime.advance().unwrap();
        assert!(
            changes.is_empty(),
            "no project should match: {:?}",
            changes.records
        );
    }

    #[test]
    fn second_advance_with_no_new_events_is_empty() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        runtime.ingest(project("p1", &["ai"])).unwrap();
        runtime.ingest(signal("ai stuff", &["ai"], 0.5)).unwrap();
        let first = runtime.advance().unwrap();
        assert!(!first.is_empty());
        let second = runtime.advance().unwrap();
        assert!(
            second.is_empty(),
            "second advance with no new events should be empty: {:?}",
            second.records
        );
    }

    #[test]
    fn inspect_reflects_derived_state_after_materialize() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        runtime.ingest(project("p1", &["ai", "voice"])).unwrap();
        runtime
            .ingest(signal("voice models", &["voice", "ai"], 0.9))
            .unwrap();
        // Under per-event scheduling (ADR-0006), state is already
        // derived — no explicit materialize step needed.
        let view = runtime.inspect_project("p1").expect("project exists");
        assert!(
            view.strategic_relevance_visible > 0.5,
            "expected strategic_relevance > 0.5 after matching signal, got {}",
            view.strategic_relevance_visible
        );
    }

    #[test]
    fn project_edit_replays_deterministically() {
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mut a = WorldRuntime::in_memory().unwrap();
        let mut b = WorldRuntime::in_memory().unwrap();

        for rt in [&mut a, &mut b] {
            rt.ingest_at(project("tnt", &["ai"]), t0).unwrap();
            rt.ingest_at(
                WorldEvent::ProjectUpdated {
                    id: "tnt".into(),
                    name: Some("TNT (voice agent)".into()),
                    tags: Some(vec!["ai".into(), "voice".into()]),
                    depends_on: None,
                },
                t0 + Duration::minutes(5),
            )
            .unwrap();
            rt.ingest_at(
                signal("voice progress", &["voice"], 0.7),
                t0 + Duration::minutes(10),
            )
            .unwrap();
        }

        // State already derived per-event under ADR-0006.

        let va = a.inspect_project("tnt").unwrap();
        let vb = b.inspect_project("tnt").unwrap();
        assert_eq!(va.name, "TNT (voice agent)");
        assert_eq!(va.tags, vec!["ai", "voice"]);
        assert!((va.strategic_relevance_visible - vb.strategic_relevance_visible).abs() < 1e-6);
    }

    #[test]
    fn cli_style_disk_roundtrip_archive_after_signal_preserves_relevance() {
        let dir = tempfile_dir();

        // First "CLI invocation": create the world.
        {
            let mut rt = WorldRuntime::open_dir(&dir).unwrap();
            rt.ingest(project("tnt", &["ai", "voice"])).unwrap();
            rt.ingest(WorldEvent::GoalCreated {
                id: "g1".into(),
                name: "Voice agent".into(),
                tags: vec!["ai".into(), "voice".into()],
                importance: 1.0,
            })
            .unwrap();
            rt.ingest(signal("voice progress", &["ai", "voice"], 0.6))
                .unwrap();
        }

        // Sleep a few millis so the archive's occurred_at is strictly
        // greater than the signal's.
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Second "CLI invocation": archive.
        {
            let mut rt = WorldRuntime::open_dir(&dir).unwrap();
            rt.ingest(WorldEvent::ProjectArchived {
                id: "tnt".into(),
                reason: Some("shipped".into()),
            })
            .unwrap();
        }

        // Third "CLI invocation": inspect. Should still show the
        // pre-archival match's effect on relevance — under per-event
        // scheduling (ADR-0006) state is derived during open_dir's
        // replay, so no materialize step is needed.
        let mut rt = WorldRuntime::open_dir(&dir).unwrap();
        let view = rt.inspect_project("tnt").expect("project exists");
        assert_eq!(view.status, ProjectStatus::Archived);
        assert!(
            view.strategic_relevance_visible > 0.5,
            "signal arrived before archive — relevance should be > 0.5, got {}",
            view.strategic_relevance_visible
        );
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let base = std::env::temp_dir();
        let unique = format!(
            "liferuntime-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        base.join(unique)
    }

    #[test]
    fn signals_arriving_before_archive_still_count_after_replay() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        runtime
            .ingest_at(project("p1", &["ai", "voice"]), t0)
            .unwrap();
        runtime
            .ingest_at(
                signal("voice progress", &["ai", "voice"], 0.6),
                t0 + Duration::minutes(1),
            )
            .unwrap();
        runtime
            .ingest_at(
                WorldEvent::ProjectArchived {
                    id: "p1".into(),
                    reason: Some("shipped".into()),
                },
                t0 + Duration::minutes(2),
            )
            .unwrap();

        // Don't advance — just materialize. Matching should run for the
        // signal because it arrived before the archive.
        // Under per-event scheduling (ADR-0006), state is already
        // derived — no explicit materialize step needed.

        let view = runtime.inspect_project("p1").expect("project exists");
        assert_eq!(view.status, ProjectStatus::Archived);
        assert!(
            view.strategic_relevance_visible > 0.5,
            "signal that arrived before archive should still have bumped relevance, got {}",
            view.strategic_relevance_visible
        );
    }

    #[test]
    fn archived_projects_are_skipped_by_matching_and_decay() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        runtime
            .ingest_at(project("p1", &["ai", "voice"]), t0)
            .unwrap();
        runtime
            .ingest_at(
                WorldEvent::ProjectArchived {
                    id: "p1".into(),
                    reason: Some("paused".into()),
                },
                t0 + Duration::minutes(1),
            )
            .unwrap();
        // Signal arrives AFTER archive — under per-event scheduling the
        // matching system sees an Archived project and skips. (Replay
        // order also yields archive-then-signal, matching the live
        // order, because each event runs the schedule against the
        // then-current world.)
        runtime
            .ingest_at(
                signal("voice models improving", &["ai", "voice"], 1.0),
                t0 + Duration::minutes(2),
            )
            .unwrap();
        // Days later, a pulse advances time → decay would normally fire.
        runtime
            .ingest_at(
                WorldEvent::TimePulseObserved {
                    observed_at: t0 + Duration::days(30),
                },
                t0 + Duration::days(30),
            )
            .unwrap();

        let changes = runtime.advance().unwrap();
        assert!(
            !changes.contains_change_for("p1"),
            "archived project should not change: {:?}",
            changes.records
        );

        let view = runtime.inspect_project("p1").expect("project exists");
        assert_eq!(view.status, ProjectStatus::Archived);
        assert_eq!(view.archived_reason.as_deref(), Some("paused"));
        assert!(
            (view.strategic_relevance_visible - 0.5).abs() < 1e-6,
            "archived project should remain at default 0.5, got {}",
            view.strategic_relevance_visible
        );
    }

    #[test]
    fn goal_amplifies_signal_matching() {
        let mut runtime_no_goal = WorldRuntime::in_memory().unwrap();
        runtime_no_goal
            .ingest(project("tnt", &["ai", "voice"]))
            .unwrap();
        runtime_no_goal
            .ingest(signal("voice progress", &["voice", "ai"], 0.6))
            .unwrap();
        let base = runtime_no_goal.advance().unwrap();
        let base_delta = base
            .records
            .iter()
            .find(|r| r.entity_id == "tnt" && r.field == "strategic_relevance")
            .map(|r| r.after - r.before)
            .expect("base run should have a relevance delta");

        let mut runtime_with_goal = WorldRuntime::in_memory().unwrap();
        runtime_with_goal
            .ingest(project("tnt", &["ai", "voice"]))
            .unwrap();
        runtime_with_goal
            .ingest(WorldEvent::GoalCreated {
                id: "voice-agent".into(),
                name: "Ship voice-first agent".into(),
                tags: vec!["ai".into(), "voice".into()],
                importance: 1.0,
            })
            .unwrap();
        runtime_with_goal
            .ingest(signal("voice progress", &["voice", "ai"], 0.6))
            .unwrap();
        let amped = runtime_with_goal.advance().unwrap();
        let amped_delta = amped
            .records
            .iter()
            .find(|r| r.entity_id == "tnt" && r.field == "strategic_relevance")
            .map(|r| r.after - r.before)
            .expect("amplified run should have a relevance delta");

        let ratio = amped_delta / base_delta;
        assert!(
            (ratio - 1.5).abs() < 1e-4,
            "max-importance goal should amplify by 1.5×, got {ratio} (amped={amped_delta}, base={base_delta})"
        );

        let rendered = amped
            .records
            .iter()
            .flat_map(|r| r.causes.iter())
            .any(|c| matches!(c, crate::Cause::GoalAmplified { .. }));
        assert!(
            rendered,
            "amplified change should carry a GoalAmplified cause"
        );
    }

    #[test]
    fn duplicate_project_id_is_rejected_at_ingest() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        let result = rt.ingest(project("tnt", &["voice"]));
        assert!(
            matches!(
                result,
                Err(RuntimeError::DuplicateEntity {
                    kind: "Project",
                    ..
                })
            ),
            "expected DuplicateEntity error, got {result:?}"
        );
    }

    #[test]
    fn edit_of_nonexistent_project_is_rejected() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        let result = rt.ingest(WorldEvent::ProjectUpdated {
            id: "nope".into(),
            name: Some("Whatever".into()),
            tags: None,
            depends_on: None,
        });
        assert!(
            matches!(
                result,
                Err(RuntimeError::EntityNotFound {
                    kind: "Project",
                    ..
                })
            ),
            "expected EntityNotFound, got {result:?}"
        );
    }

    #[test]
    fn out_of_range_confidence_is_rejected() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        let result = rt.ingest(WorldEvent::SignalObserved {
            source: "x".into(),
            summary: "x".into(),
            tags: vec!["ai".into()],
            confidence: 5.0,
            observed_at: None,
        });
        assert!(
            matches!(
                result,
                Err(RuntimeError::ValueOutOfRange {
                    field: "confidence",
                    ..
                })
            ),
            "expected ValueOutOfRange, got {result:?}"
        );
    }

    #[test]
    fn pending_events_reports_unadvanced_count() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("p", &["ai"])).unwrap();
        rt.ingest(signal("s1", &["ai"], 0.5)).unwrap();
        assert_eq!(rt.pending_events().unwrap(), 2);
        rt.advance().unwrap();
        assert_eq!(rt.pending_events().unwrap(), 0);
        rt.ingest(signal("s2", &["ai"], 0.5)).unwrap();
        assert_eq!(rt.pending_events().unwrap(), 1);
    }

    #[test]
    fn trajectories_show_most_recent_delta_not_window_average() {
        let dir = tempfile_dir();
        {
            let mut rt = WorldRuntime::open_dir(&dir).unwrap();
            rt.ingest_at(
                project("hot", &["ai"]),
                Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            )
            .unwrap();
            rt.ingest_at(
                signal("ai news", &["ai"], 1.0),
                Utc.with_ymd_and_hms(2026, 1, 1, 0, 1, 0).unwrap(),
            )
            .unwrap();
            rt.advance().unwrap(); // first advance: bump
            rt.ingest_at(
                WorldEvent::TimePulseObserved {
                    observed_at: Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap(),
                },
                Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap(),
            )
            .unwrap();
            rt.advance().unwrap(); // second advance: decay
        }

        let mut rt = WorldRuntime::open_dir(&dir).unwrap();
        let trajectories = rt.trajectories(5).unwrap();
        let hot = trajectories
            .iter()
            .find(|t| t.id == "hot")
            .expect("project visible");
        // Most recent advance was the decay; slope should be negative.
        assert!(
            hot.slope_relevance < 0.0,
            "expected cooling slope, got {} (window-average would be ~zero)",
            hot.slope_relevance
        );
    }

    #[test]
    fn idempotency_key_dedupes_repeated_ingest() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();

        let s = signal("ai news", &["ai"], 0.8);
        let first = rt
            .ingest_with_key(s.clone(), Some("cron-2026-05-17".into()))
            .unwrap();
        let second = rt
            .ingest_with_key(s.clone(), Some("cron-2026-05-17".into()))
            .unwrap();

        assert_eq!(
            first.event_id, second.event_id,
            "second ingest with same key should return the original event id"
        );

        // Bump should only have happened once.
        let changes = rt.advance().unwrap();
        let bumps: Vec<_> = changes
            .records
            .iter()
            .filter(|r| r.entity_id == "tnt" && r.field == "strategic_relevance")
            .collect();
        assert_eq!(
            bumps.len(),
            1,
            "expected exactly one strategic_relevance bump, got {bumps:#?}"
        );
    }

    #[test]
    fn idempotency_keys_persist_across_open_dir() {
        let dir = tempfile_dir();

        {
            let mut rt = WorldRuntime::open_dir(&dir).unwrap();
            rt.ingest(project("tnt", &["ai"])).unwrap();
            rt.ingest_with_key(signal("ai news", &["ai"], 0.6), Some("kron-job-42".into()))
                .unwrap();
        }

        // Second "CLI invocation" with the same key should be a no-op.
        let mut rt = WorldRuntime::open_dir(&dir).unwrap();
        let before_advance = rt.event_count();
        rt.ingest_with_key(signal("ai news", &["ai"], 0.6), Some("kron-job-42".into()))
            .unwrap();
        let after_advance = rt.event_count();
        assert_eq!(
            before_advance, after_advance,
            "ingest with seen key should not append to log"
        );
    }

    #[test]
    fn tag_matching_is_canonical_case_separator_insensitive() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(WorldEvent::ProjectCreated {
            id: "tnt".into(),
            name: "TNT".into(),
            tags: vec!["ai-voice".into()],
        })
        .unwrap();
        // Signal uses a different separator and case — should still match.
        rt.ingest(signal("voice models", &["AI Voice"], 0.6))
            .unwrap();
        let changes = rt.advance().unwrap();
        assert!(
            changes.contains_change_for("tnt"),
            "AI Voice should canonicalize to ai-voice and match: {:?}",
            changes.records
        );
    }

    #[test]
    fn achieved_goal_no_longer_amplifies_matching() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        runtime.ingest(project("tnt", &["ai", "voice"])).unwrap();
        runtime
            .ingest(WorldEvent::GoalCreated {
                id: "g1".into(),
                name: "Ship voice agent".into(),
                tags: vec!["ai".into(), "voice".into()],
                importance: 1.0,
            })
            .unwrap();
        runtime
            .ingest(signal("voice progress", &["voice", "ai"], 0.6))
            .unwrap();
        let first = runtime.advance().unwrap();
        let amped_delta = first
            .records
            .iter()
            .find(|r| r.entity_id == "tnt" && r.field == "strategic_relevance")
            .map(|r| r.after - r.before)
            .expect("amplified bump exists");

        // Achieve the goal; subsequent signal should NOT be amplified.
        runtime
            .ingest(WorldEvent::GoalAchieved {
                id: "g1".into(),
                note: Some("shipped v1".into()),
            })
            .unwrap();
        runtime
            .ingest(signal("more voice news", &["voice", "ai"], 0.6))
            .unwrap();
        let second = runtime.advance().unwrap();
        let unamped_delta = second
            .records
            .iter()
            .find(|r| r.entity_id == "tnt" && r.field == "strategic_relevance")
            .map(|r| r.after - r.before)
            .expect("post-achievement bump exists");

        // Ratio of amped : unamped should be ~1.5.
        let ratio = amped_delta / unamped_delta;
        assert!(
            (ratio - 1.5).abs() < 1e-3,
            "achieved goal should no longer amplify (ratio expected ~1.5, got {ratio})"
        );
    }

    #[test]
    fn time_pulse_advances_event_log_time_and_fires_decay() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        runtime.ingest_at(project("p1", &["ai"]), t0).unwrap();
        runtime
            .ingest_at(
                signal("strong ai signal", &["ai"], 1.0),
                t0 + Duration::minutes(1),
            )
            .unwrap();
        runtime.advance().unwrap();

        // No real-world events, but a pulse moves time forward 14 days.
        runtime
            .ingest_at(
                WorldEvent::TimePulseObserved {
                    observed_at: t0 + Duration::days(14),
                },
                t0 + Duration::days(14),
            )
            .unwrap();
        let changes = runtime.advance().unwrap();

        let decay = changes
            .records
            .iter()
            .find(|r| r.entity_id == "p1" && r.field == "strategic_relevance")
            .expect("pulse-driven decay should produce a record");
        assert!(
            decay.after < decay.before,
            "pulse should pull strategic_relevance toward baseline"
        );
    }

    #[test]
    fn decay_pulls_strategic_relevance_back_toward_baseline_over_event_time() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        runtime
            .ingest_at(project("p1", &["ai", "voice"]), t0)
            .unwrap();
        runtime
            .ingest_at(
                signal("strong voice signal", &["ai", "voice"], 1.0),
                t0 + Duration::minutes(1),
            )
            .unwrap();
        let first = runtime.advance().unwrap();
        let rel_after_match = first
            .records
            .iter()
            .find(|r| r.entity_id == "p1" && r.field == "strategic_relevance")
            .map(|r| r.after)
            .expect("match should produce a strategic_relevance change");
        assert!(rel_after_match > 0.5);

        // 30 event-log days pass with an unrelated signal landing at t0+30d.
        runtime
            .ingest_at(
                signal("political news", &["politics"], 0.5),
                t0 + Duration::days(30),
            )
            .unwrap();
        let second = runtime.advance().unwrap();

        let decay = second
            .records
            .iter()
            .find(|r| r.entity_id == "p1" && r.field == "strategic_relevance")
            .expect("decay should produce a strategic_relevance change");
        assert!(
            decay.after < decay.before,
            "expected decay to pull value down: before={}, after={}",
            decay.before,
            decay.after
        );
        assert!(
            decay.after > 0.5,
            "decay should approach 0.5 baseline asymptotically, not undershoot: {}",
            decay.after
        );
    }

    // -------- Decision: event-log tracer (issue #1) --------
    //
    // Per ADR-0008 + ADR-0005, `DecisionRecorded` is an additive event
    // variant. This first slice plumbs the variant end-to-end with **no
    // system effects yet** — derived state must be unchanged. Later
    // slices (issues #2..#7) add stance derivation, boost, and
    // dampening.

    #[test]
    fn decision_recorded_does_not_change_raw_relevance() {
        // ADR-0008 #chosen-decaying-boost-not-a-floor: a Decision adds
        // an additive boost on *visible* relevance, never touches the
        // raw field. This pins the raw-immutability invariant; the
        // visible-side effect is covered by the issue #4 boost tests.
        let mut runtime = WorldRuntime::in_memory().unwrap();
        runtime.ingest(project("tnt", &["ai"])).unwrap();
        let before = runtime.inspect_project("tnt").expect("project exists");
        runtime
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec![],
                reason: Some("focus".into()),
                decided_at: None,
            })
            .unwrap();
        let after = runtime
            .inspect_project("tnt")
            .expect("project still exists");
        assert!(
            (before.strategic_relevance_raw - after.strategic_relevance_raw).abs() < 1e-6,
            "raw relevance must not move on Decision: {} → {}",
            before.strategic_relevance_raw,
            after.strategic_relevance_raw,
        );
        assert!((before.urgency - after.urgency).abs() < 1e-6);
        assert_eq!(before.status, after.status);
    }

    #[test]
    fn decision_recorded_rejects_unknown_chose() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        runtime.ingest(project("tnt", &["ai"])).unwrap();
        let result = runtime.ingest(WorldEvent::DecisionRecorded {
            chose: "ghost".into(),
            over: vec![],
            dampen: vec![],
            reason: None,
            decided_at: None,
        });
        assert!(
            matches!(
                result,
                Err(RuntimeError::EntityNotFound {
                    kind: "Project",
                    ..
                })
            ),
            "expected EntityNotFound for unknown chose, got {result:?}"
        );
        // No event appended on rejection.
        assert_eq!(runtime.event_count(), 1);
    }

    #[test]
    fn decision_recorded_rejects_unknown_over() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        runtime.ingest(project("tnt", &["ai"])).unwrap();
        let result = runtime.ingest(WorldEvent::DecisionRecorded {
            chose: "tnt".into(),
            over: vec!["ghost".into()],
            dampen: vec![],
            reason: None,
            decided_at: None,
        });
        assert!(
            matches!(
                result,
                Err(RuntimeError::EntityNotFound {
                    kind: "Project",
                    ..
                })
            ),
            "expected EntityNotFound for unknown over, got {result:?}"
        );
        assert_eq!(runtime.event_count(), 1);
    }

    #[test]
    fn decision_recorded_rejects_unknown_dampen() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        runtime.ingest(project("tnt", &["ai"])).unwrap();
        let result = runtime.ingest(WorldEvent::DecisionRecorded {
            chose: "tnt".into(),
            over: vec![],
            dampen: vec!["ghost".into()],
            reason: None,
            decided_at: None,
        });
        assert!(
            matches!(
                result,
                Err(RuntimeError::EntityNotFound {
                    kind: "Project",
                    ..
                })
            ),
            "expected EntityNotFound for unknown dampen, got {result:?}"
        );
        assert_eq!(runtime.event_count(), 1);
    }

    #[test]
    fn decision_recorded_jsonl_roundtrip_replay_unchanged() {
        let dir = tempfile_dir();

        // First "CLI invocation": record projects and a decision.
        {
            let mut rt = WorldRuntime::open_dir(&dir).unwrap();
            rt.ingest(project("tnt", &["ai", "voice"])).unwrap();
            rt.ingest(project("side-x", &["voice"])).unwrap();
            rt.ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec!["side-x".into()],
                dampen: vec!["side-x".into()],
                reason: Some("ship voice first".into()),
                decided_at: None,
            })
            .unwrap();
        }

        // Re-open: replay must succeed without panic, the event count
        // reflects the persisted decision, and derived state matches
        // the no-decision baseline (per-event scheduling derived nothing
        // from the tracer in this slice).
        let mut rt = WorldRuntime::open_dir(&dir).unwrap();
        assert_eq!(
            rt.event_count(),
            3,
            "two projects + one decision should be persisted"
        );
        let tnt = rt.inspect_project("tnt").expect("tnt persisted");
        let side = rt.inspect_project("side-x").expect("side-x persisted");
        // Raw relevance is untouched by Decisions (boost is an additive
        // layer surfaced through `_visible`; per ADR-0008). Issue #4
        // adds the boost; this assertion now pins the additive-only
        // invariant rather than "Decision changes nothing."
        assert!(
            (tnt.strategic_relevance_raw - 0.5).abs() < 1e-6,
            "raw relevance unchanged by Decision: {}",
            tnt.strategic_relevance_raw
        );
        assert!((side.strategic_relevance_raw - 0.5).abs() < 1e-6);
    }

    // -------- Decision: per-project stance derivation (issue #2) --------

    use crate::model::DecisionStance;

    #[test]
    fn decision_stance_single_chosen_and_dampened() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("side-x", &["voice"])).unwrap();

        let receipt = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec!["side-x".into()],
                dampen: vec!["side-x".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();

        let stances = rt.decision_stances();
        assert_eq!(
            stances.get("tnt"),
            Some(&DecisionStance::Chosen {
                decision_id: receipt.event_id.clone()
            }),
            "tnt should be Chosen by the new decision, got {stances:?}",
        );
        assert_eq!(
            stances.get("side-x"),
            Some(&DecisionStance::Dampened {
                decision_id: receipt.event_id.clone()
            }),
            "side-x should be Dampened by the new decision",
        );
    }

    #[test]
    fn decision_stance_partial_supersession() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("side-x", &["voice"])).unwrap();

        let dec_a = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec!["side-x".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        let dec_b = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "side-x".into(),
                over: vec![],
                dampen: vec![],
                reason: None,
                decided_at: None,
            })
            .unwrap();

        let stances = rt.decision_stances();
        assert_eq!(
            stances.get("tnt"),
            Some(&DecisionStance::Chosen {
                decision_id: dec_a.event_id.clone()
            }),
            "tnt should still be steered by Decision A (partial supersession): {stances:?}",
        );
        assert_eq!(
            stances.get("side-x"),
            Some(&DecisionStance::Chosen {
                decision_id: dec_b.event_id.clone()
            }),
            "side-x should now be steered by Decision B (overrides A's dampen)",
        );
    }

    #[test]
    fn decision_stance_chose_then_dampen_flips_stance() {
        // Decision A chose: tnt; later Decision B dampens tnt.
        // tnt's stance flips to Dampened by B.
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("y", &[])).unwrap();

        let _dec_a = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec!["y".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        let dec_b = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "y".into(),
                over: vec![],
                dampen: vec!["tnt".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();

        let stances = rt.decision_stances();
        assert_eq!(
            stances.get("tnt"),
            Some(&DecisionStance::Dampened {
                decision_id: dec_b.event_id.clone()
            })
        );
        assert_eq!(
            stances.get("y"),
            Some(&DecisionStance::Chosen {
                decision_id: dec_b.event_id.clone()
            })
        );
    }

    #[test]
    fn decision_stance_same_decided_at_uses_replay_order() {
        let t = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest_at(project("tnt", &["ai"]), t).unwrap();

        let dec_a = rt
            .ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "tnt".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: Some(t),
                },
                t + Duration::seconds(1),
            )
            .unwrap();
        let dec_b = rt
            .ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "tnt".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: Some(t),
                },
                t + Duration::seconds(2),
            )
            .unwrap();

        let stances = rt.decision_stances();
        assert_eq!(
            stances.get("tnt"),
            Some(&DecisionStance::Chosen {
                decision_id: dec_b.event_id.clone()
            }),
            "later-in-log Decision should win even with identical decided_at",
        );
        assert!(
            !stances.values().any(|s| {
                let id = match s {
                    DecisionStance::Chosen { decision_id }
                    | DecisionStance::Dampened { decision_id } => decision_id,
                };
                id == &dec_a.event_id
            }),
            "Decision A should no longer steer any project: {stances:?}",
        );
    }

    #[test]
    fn decision_stance_persists_through_open_dir_replay() {
        let dir = tempfile_dir();

        let dec_id_str;
        {
            let mut rt = WorldRuntime::open_dir(&dir).unwrap();
            rt.ingest(project("tnt", &["ai"])).unwrap();
            let dec = rt
                .ingest(WorldEvent::DecisionRecorded {
                    chose: "tnt".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: None,
                })
                .unwrap();
            dec_id_str = dec.event_id.0.clone();
        }

        let mut rt = WorldRuntime::open_dir(&dir).unwrap();
        let stances = rt.decision_stances();
        match stances.get("tnt") {
            Some(DecisionStance::Chosen { decision_id }) => {
                assert_eq!(decision_id.0, dec_id_str);
            }
            other => panic!("expected Chosen after replay, got {other:?}"),
        }
    }

    // -------- Decision: revocation (issue #3) --------

    #[test]
    fn decision_revoke_clears_stances_owned_by_that_decision() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("side-x", &["voice"])).unwrap();

        let dec = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec!["side-x".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();

        let pre = rt.decision_stances();
        assert!(pre.contains_key("tnt") && pre.contains_key("side-x"));

        rt.ingest(WorldEvent::DecisionRevoked {
            decision_id: dec.event_id.clone(),
        })
        .unwrap();

        let post = rt.decision_stances();
        assert!(
            post.is_empty(),
            "revoke should clear every project this Decision steered: {post:?}",
        );
    }

    #[test]
    fn decision_revoke_partial_only_clears_projects_still_owned_by_that_decision() {
        // A steers tnt + dampens side-x. B then takes over tnt.
        // After A+B: tnt=Chosen{B}, side-x=Dampened{A}.
        // Revoke A → tnt unchanged (still B), side-x cleared.
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("side-x", &["voice"])).unwrap();

        let dec_a = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec!["side-x".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        let dec_b = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec![],
                reason: None,
                decided_at: None,
            })
            .unwrap();

        rt.ingest(WorldEvent::DecisionRevoked {
            decision_id: dec_a.event_id.clone(),
        })
        .unwrap();

        let stances = rt.decision_stances();
        assert_eq!(
            stances.get("tnt"),
            Some(&DecisionStance::Chosen {
                decision_id: dec_b.event_id.clone()
            }),
            "tnt should remain steered by B (A no longer owned it)",
        );
        assert!(
            !stances.contains_key("side-x"),
            "side-x's Dampened{{A}} should be cleared by A's revoke: {stances:?}",
        );
    }

    #[test]
    fn decision_revoke_orphan_id_is_rejected_at_ingest() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        let before = rt.event_count();
        let result = rt.ingest(WorldEvent::DecisionRevoked {
            decision_id: EventId("never-recorded".into()),
        });
        assert!(
            matches!(
                result,
                Err(RuntimeError::EntityNotFound {
                    kind: "Decision",
                    ..
                })
            ),
            "expected EntityNotFound for orphan revoke, got {result:?}",
        );
        assert_eq!(
            rt.event_count(),
            before,
            "rejection must not append to the log"
        );
    }

    #[test]
    fn decision_revoke_orphan_in_corrupted_log_is_silently_ignored_on_replay() {
        // ADR-0008 (amended): replay silently tolerates an orphan
        // DecisionRevoked. Manually append a revoke for a never-recorded
        // decision id and re-open.
        let dir = tempfile_dir();
        {
            let _rt = WorldRuntime::open_dir(&dir).unwrap();
        }

        let path = dir.join("events.jsonl");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let bogus = StoredEvent {
            id: EventId::new(),
            occurred_at: Utc::now(),
            idempotency_key: None,
            payload: WorldEvent::DecisionRevoked {
                decision_id: EventId("01HZNEVERRECORDED0000000000".into()),
            },
        };
        std::io::Write::write_all(
            &mut f,
            format!("{}\n", serde_json::to_string(&bogus).unwrap()).as_bytes(),
        )
        .unwrap();
        drop(f);

        let mut rt = WorldRuntime::open_dir(&dir).unwrap();
        let stances = rt.decision_stances();
        assert!(stances.is_empty());
    }

    #[test]
    fn decision_revoke_then_record_new_decision_resteers_cleanly() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        let dec_a = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec![],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        rt.ingest(WorldEvent::DecisionRevoked {
            decision_id: dec_a.event_id.clone(),
        })
        .unwrap();
        assert!(rt.decision_stances().is_empty());

        let dec_b = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec![],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        assert_eq!(
            rt.decision_stances().get("tnt"),
            Some(&DecisionStance::Chosen {
                decision_id: dec_b.event_id
            }),
        );
    }

    #[test]
    fn pre_decision_log_still_replays_with_new_variant_present() {
        // Additive-variant guarantee (ADR-0005): a log written before
        // Decision events existed must replay byte-identically into a
        // binary that knows about DecisionRecorded. The new variant
        // simply doesn't appear in the log, so replay should match what
        // the runtime produced previously.
        let dir = tempfile_dir();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        {
            let mut rt = WorldRuntime::open_dir(&dir).unwrap();
            rt.ingest_at(project("tnt", &["ai", "voice"]), t0).unwrap();
            rt.ingest_at(
                signal("voice progress", &["voice", "ai"], 0.6),
                t0 + Duration::minutes(1),
            )
            .unwrap();
        }

        let mut rt = WorldRuntime::open_dir(&dir).unwrap();
        let tnt = rt.inspect_project("tnt").expect("project exists");
        assert!(
            tnt.strategic_relevance_visible > 0.5,
            "existing matching behavior must survive the additive variant addition, got {}",
            tnt.strategic_relevance_visible
        );
    }

    // -------- Decision: Chosen — decaying boost + ProjectView raw/visible split (issue #4) --------

    #[test]
    fn decision_chose_applies_initial_boost_of_0_15_to_visible_relevance() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        let before = rt.inspect_project("tnt").unwrap();
        assert!(
            (before.strategic_relevance_visible - before.strategic_relevance_raw).abs() < 1e-6,
            "with no boost, visible should equal raw"
        );

        rt.ingest(WorldEvent::DecisionRecorded {
            chose: "tnt".into(),
            over: vec![],
            dampen: vec![],
            reason: None,
            decided_at: None,
        })
        .unwrap();

        let after = rt.inspect_project("tnt").unwrap();
        assert!(
            (after.strategic_relevance_raw - before.strategic_relevance_raw).abs() < 1e-6,
            "raw must NOT change on Decision: before={} after={}",
            before.strategic_relevance_raw,
            after.strategic_relevance_raw,
        );
        assert!(
            (after.strategic_relevance_visible - (before.strategic_relevance_raw + 0.15)).abs()
                < 1e-4,
            "visible should be raw + 0.15 boost, got visible={} (raw={})",
            after.strategic_relevance_visible,
            after.strategic_relevance_raw,
        );
    }

    #[test]
    fn decision_boost_decays_over_event_log_days() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        rt.ingest_at(project("tnt", &["ai"]), t0).unwrap();
        rt.ingest_at(
            WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec![],
                reason: None,
                decided_at: Some(t0),
            },
            t0,
        )
        .unwrap();

        let immediately_after = rt.inspect_project("tnt").unwrap();
        let visible_at_apply = immediately_after.strategic_relevance_visible;

        // Advance event-log time 30 days via a TimePulse (ADR-0004).
        rt.ingest_at(
            WorldEvent::TimePulseObserved {
                observed_at: t0 + Duration::days(30),
            },
            t0 + Duration::days(30),
        )
        .unwrap();

        let after_decay = rt.inspect_project("tnt").unwrap();
        let visible_after_decay = after_decay.strategic_relevance_visible;

        assert!(
            visible_after_decay < visible_at_apply,
            "boost should monotonically decay toward 0 (0.999/day): visible_at_apply={visible_at_apply} after_30d={visible_after_decay}",
        );
        // 0.999^30 ≈ 0.9704. Initial boost 0.15 → ≈ 0.1456 remaining.
        // Visible erosion should be ≈ 0.0044 — well above any noise floor.
        let erosion = visible_at_apply - visible_after_decay;
        assert!(
            erosion > 0.001,
            "expected ≥0.001 erosion after 30 days, got {erosion}"
        );
        // Raw stays put.
        assert!(
            (after_decay.strategic_relevance_raw - immediately_after.strategic_relevance_raw).abs()
                < 1e-6,
            "raw must not change under decay of boost"
        );
    }

    #[test]
    fn decision_boost_emits_decision_boost_applied_cause_on_apply() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        rt.ingest_at(project("tnt", &["ai"]), t0).unwrap();
        let dec = rt
            .ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "tnt".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: Some(t0),
                },
                t0,
            )
            .unwrap();

        let changes = rt.advance().unwrap();
        let cited = changes.records.iter().any(|r| {
            r.entity_id == "tnt"
                && r.field == "strategic_relevance"
                && r.causes.iter().any(|c| {
                    matches!(
                        c,
                        crate::Cause::DecisionBoostApplied { decision_id, .. }
                            if decision_id == &dec.event_id
                    )
                })
        });
        assert!(
            cited,
            "initial boost should emit a DecisionBoostApplied cause citing decision id: {:#?}",
            changes.records,
        );
    }

    #[test]
    fn decision_boost_emits_decision_boost_applied_cause_on_decay() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        rt.ingest_at(project("tnt", &["ai"]), t0).unwrap();
        let dec = rt
            .ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "tnt".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: Some(t0),
                },
                t0,
            )
            .unwrap();
        rt.advance().unwrap();

        rt.ingest_at(
            WorldEvent::TimePulseObserved {
                observed_at: t0 + Duration::days(30),
            },
            t0 + Duration::days(30),
        )
        .unwrap();
        let after = rt.advance().unwrap();
        let cited = after.records.iter().any(|r| {
            r.entity_id == "tnt"
                && r.causes.iter().any(|c| {
                    matches!(
                        c,
                        crate::Cause::DecisionBoostApplied { decision_id, .. }
                            if decision_id == &dec.event_id
                    )
                })
        });
        assert!(
            cited,
            "decay should emit a DecisionBoostApplied cause: {:#?}",
            after.records,
        );
    }

    #[test]
    fn visible_relevance_clamps_to_1_when_raw_plus_boost_exceeds_1() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("p", &["ai"])).unwrap();
        rt.ingest(WorldEvent::GoalCreated {
            id: "g".into(),
            name: "g".into(),
            tags: vec!["ai".into()],
            importance: 1.0,
        })
        .unwrap();
        // Several strong signals to push raw close to 1.0.
        for _ in 0..30 {
            rt.ingest(signal("ai surge", &["ai"], 1.0)).unwrap();
        }
        let after_signals = rt.inspect_project("p").unwrap();
        assert!(
            after_signals.strategic_relevance_raw > 0.95,
            "raw should approach 1.0 after sustained signals, got {}",
            after_signals.strategic_relevance_raw
        );

        rt.ingest(WorldEvent::DecisionRecorded {
            chose: "p".into(),
            over: vec![],
            dampen: vec![],
            reason: None,
            decided_at: None,
        })
        .unwrap();

        let after_boost = rt.inspect_project("p").unwrap();
        assert!(
            after_boost.strategic_relevance_visible <= 1.0 + 1e-6,
            "visible must clamp to 1.0, got {}",
            after_boost.strategic_relevance_visible,
        );
        assert!(
            after_boost.strategic_relevance_visible >= after_boost.strategic_relevance_raw - 1e-6,
            "visible (clamped) should not drop below raw (raw={}, visible={})",
            after_boost.strategic_relevance_raw,
            after_boost.strategic_relevance_visible,
        );
    }

    #[test]
    fn explain_project_cites_decision_by_id_when_boost_contributed() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        rt.ingest_at(project("tnt", &["ai"]), t0).unwrap();
        let dec = rt
            .ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "tnt".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: Some(t0),
                },
                t0,
            )
            .unwrap();
        let explanation = rt.explain(ExplainTarget::Entity("tnt".into())).unwrap();
        let rendered = explanation.to_string();
        assert!(
            rendered.contains(dec.event_id.as_str()),
            "explanation should cite decision id {}, got:\n{rendered}",
            dec.event_id,
        );
    }

    // -------- Decision: Dampened — ×0.3 resonance + goal-amp suppression (issue #5) --------
    //
    // Per ADR-0008 `#dampened-x03-with-goal-amp-suppressed`:
    //   - For projects whose stance is `Dampened`, the matching system
    //     scales resonance deltas by `0.3`.
    //   - Goal amplification does **not** apply to dampened projects
    //     (mutually exclusive — the user's explicit per-project opt-in
    //     beats implicit goal-tag overlap).
    //   - Non-dampened projects retain their existing amplification
    //     behaviour.

    fn relevance_delta_for(records: &[ChangeRecord], entity: &str) -> f32 {
        records
            .iter()
            .find(|r| r.entity_id == entity && r.field == "strategic_relevance")
            .map(|r| r.after - r.before)
            .expect("strategic_relevance change present")
    }

    #[test]
    fn dampened_signal_scales_resonance_by_0_3() {
        // Baseline: identical project + signal in a no-decision runtime.
        let mut base_rt = WorldRuntime::in_memory().unwrap();
        base_rt.ingest(project("tnt", &["ai"])).unwrap();
        base_rt.ingest(signal("ai news", &["ai"], 0.8)).unwrap();
        let base_records = base_rt.advance().unwrap();
        let base_delta = relevance_delta_for(&base_records.records, "tnt");

        // Dampened version: identical project, dampened by a Decision.
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        // Need a `chose` target to satisfy ADR-0008's required field.
        rt.ingest(project("focus", &[])).unwrap();
        rt.ingest(WorldEvent::DecisionRecorded {
            chose: "focus".into(),
            over: vec![],
            dampen: vec!["tnt".into()],
            reason: None,
            decided_at: None,
        })
        .unwrap();
        rt.ingest(signal("ai news", &["ai"], 0.8)).unwrap();
        let damp_records = rt.advance().unwrap();
        let damp_delta = relevance_delta_for(&damp_records.records, "tnt");

        let ratio = damp_delta / base_delta;
        assert!(
            (ratio - 0.3).abs() < 1e-4,
            "dampened resonance should be ×0.3 of base: base={base_delta} damp={damp_delta} ratio={ratio}",
        );
    }

    #[test]
    fn dampened_project_skips_goal_amplification() {
        // Goal-only baseline (no dampening): factor 1.5 over base.
        let mut base_rt = WorldRuntime::in_memory().unwrap();
        base_rt.ingest(project("tnt", &["ai"])).unwrap();
        base_rt
            .ingest(WorldEvent::GoalCreated {
                id: "g".into(),
                name: "g".into(),
                tags: vec!["ai".into()],
                importance: 1.0,
            })
            .unwrap();
        base_rt.ingest(signal("ai news", &["ai"], 0.8)).unwrap();
        let base_records = base_rt.advance().unwrap();
        let base_amped_delta = relevance_delta_for(&base_records.records, "tnt");
        // Without dampening, a max-importance overlapping Goal gives ×1.5.
        let raw_delta = base_amped_delta / 1.5;

        // Same goal + dampening on tnt → goal amplification suppressed.
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("focus", &[])).unwrap();
        rt.ingest(WorldEvent::GoalCreated {
            id: "g".into(),
            name: "g".into(),
            tags: vec!["ai".into()],
            importance: 1.0,
        })
        .unwrap();
        rt.ingest(WorldEvent::DecisionRecorded {
            chose: "focus".into(),
            over: vec![],
            dampen: vec!["tnt".into()],
            reason: None,
            decided_at: None,
        })
        .unwrap();
        rt.ingest(signal("ai news", &["ai"], 0.8)).unwrap();
        let damp_records = rt.advance().unwrap();
        let damp_delta = relevance_delta_for(&damp_records.records, "tnt");

        // Expected: raw_delta × 0.3 — no goal-amp factor in the chain.
        let expected = raw_delta * 0.3;
        assert!(
            (damp_delta - expected).abs() < 1e-4,
            "dampened + goal should still be ×0.3 of raw (no amp): expected≈{expected}, got {damp_delta}",
        );

        // Also: no GoalAmplified cause on the dampened record.
        let has_goal_amp = damp_records
            .records
            .iter()
            .filter(|r| r.entity_id == "tnt")
            .flat_map(|r| r.causes.iter())
            .any(|c| matches!(c, crate::Cause::GoalAmplified { .. }));
        assert!(
            !has_goal_amp,
            "dampened project must not carry a GoalAmplified cause: {:?}",
            damp_records.records,
        );
    }

    #[test]
    fn non_dampened_project_with_matching_goal_still_amplifies() {
        // Control: same setup as above but project `tnt` is NOT in
        // anyone's dampen list. Goal amp should fire (×1.5).
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("other", &[])).unwrap();
        rt.ingest(WorldEvent::GoalCreated {
            id: "g".into(),
            name: "g".into(),
            tags: vec!["ai".into()],
            importance: 1.0,
        })
        .unwrap();
        rt.ingest(WorldEvent::DecisionRecorded {
            chose: "other".into(),
            over: vec![],
            dampen: vec![],
            reason: None,
            decided_at: None,
        })
        .unwrap();
        rt.ingest(signal("ai news", &["ai"], 0.8)).unwrap();
        let records = rt.advance().unwrap();
        let has_goal_amp = records
            .records
            .iter()
            .filter(|r| r.entity_id == "tnt")
            .flat_map(|r| r.causes.iter())
            .any(|c| matches!(c, crate::Cause::GoalAmplified { .. }));
        assert!(
            has_goal_amp,
            "non-dampened project with matching goal should carry GoalAmplified",
        );
    }

    #[test]
    fn dampened_change_record_cites_decision_dampened_cause() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("focus", &[])).unwrap();
        let dec = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "focus".into(),
                over: vec![],
                dampen: vec!["tnt".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        rt.ingest(signal("ai news", &["ai"], 0.8)).unwrap();
        let records = rt.advance().unwrap();

        let cited = records.records.iter().any(|r| {
            r.entity_id == "tnt"
                && r.field == "strategic_relevance"
                && r.causes.iter().any(|c| {
                    matches!(
                        c,
                        crate::Cause::DecisionDampened { decision_id, factor }
                            if decision_id == &dec.event_id && (*factor - 0.3).abs() < 1e-6
                    )
                })
        });
        assert!(
            cited,
            "dampened change record should carry DecisionDampened cause: {:?}",
            records.records,
        );

        let rendered = rt
            .explain(ExplainTarget::Entity("tnt".into()))
            .unwrap()
            .to_string();
        assert!(
            rendered.contains(dec.event_id.as_str()),
            "explain should cite the dampening decision id, got:\n{rendered}",
        );
    }

    // -------- Decision: stance flip on supersession (issue #6) --------
    //
    // Per ADR-0008 `#per-project-stance-derived-by-replay`:
    //   - Chosen → Dampened: drop the boost component; matching now
    //     applies ×0.3 to resonance deltas.
    //   - Dampened → Chosen: start a *fresh* `+0.15` boost owned by
    //     the new Decision.
    //   - Chosen → Chosen (same project): boost resets to `+0.15` —
    //     boosts do NOT stack.
    //   - Dampened → Dampened (same project): stance.decision_id
    //     updates; mechanic stays ×0.3.

    #[test]
    fn stance_flip_chose_then_dampen_drops_boost() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("other", &[])).unwrap();

        let _dec_a = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec![],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        let view_chose = rt.inspect_project("tnt").unwrap();
        assert!(
            (view_chose.strategic_relevance_visible - view_chose.strategic_relevance_raw - 0.15)
                .abs()
                < 1e-4,
            "Chosen: visible should equal raw + 0.15"
        );

        let dec_b = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "other".into(),
                over: vec![],
                dampen: vec!["tnt".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();

        let view_dampen = rt.inspect_project("tnt").unwrap();
        assert!(
            (view_dampen.strategic_relevance_visible - view_dampen.strategic_relevance_raw).abs()
                < 1e-6,
            "Dampened: visible should drop back to raw (boost dropped): raw={} visible={}",
            view_dampen.strategic_relevance_raw,
            view_dampen.strategic_relevance_visible,
        );

        let stances = rt.decision_stances();
        assert_eq!(
            stances.get("tnt"),
            Some(&DecisionStance::Dampened {
                decision_id: dec_b.event_id.clone()
            }),
        );
    }

    #[test]
    fn stance_flip_dampen_then_chose_starts_fresh_boost() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("other", &[])).unwrap();

        let _dec_a = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "other".into(),
                over: vec![],
                dampen: vec!["tnt".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        let view_after_dampen = rt.inspect_project("tnt").unwrap();
        assert!(
            (view_after_dampen.strategic_relevance_visible
                - view_after_dampen.strategic_relevance_raw)
                .abs()
                < 1e-6,
            "no boost while Dampened",
        );

        let dec_b = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec![],
                reason: None,
                decided_at: None,
            })
            .unwrap();

        let view_after_chose = rt.inspect_project("tnt").unwrap();
        let contribution =
            view_after_chose.strategic_relevance_visible - view_after_chose.strategic_relevance_raw;
        assert!(
            (contribution - 0.15).abs() < 1e-4,
            "Dampened→Chosen should start fresh +0.15 boost owned by B, got {contribution}",
        );

        let stances = rt.decision_stances();
        assert_eq!(
            stances.get("tnt"),
            Some(&DecisionStance::Chosen {
                decision_id: dec_b.event_id.clone()
            }),
        );
    }

    #[test]
    fn two_consecutive_chose_on_same_project_do_not_stack_boost() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        rt.ingest_at(project("tnt", &["ai"]), t0).unwrap();
        let _dec_a = rt
            .ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "tnt".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: None,
                },
                t0,
            )
            .unwrap();
        // Let 30 days erode A's boost.
        rt.ingest_at(
            WorldEvent::TimePulseObserved {
                observed_at: t0 + Duration::days(30),
            },
            t0 + Duration::days(30),
        )
        .unwrap();
        let mid = rt.inspect_project("tnt").unwrap();
        let mid_boost = mid.strategic_relevance_visible - mid.strategic_relevance_raw;
        assert!(
            mid_boost > 0.0 && mid_boost < 0.15,
            "A's boost should have decayed below 0.15, got {mid_boost}",
        );

        let dec_b = rt
            .ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "tnt".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: None,
                },
                t0 + Duration::days(30),
            )
            .unwrap();

        let after_b = rt.inspect_project("tnt").unwrap();
        let new_boost = after_b.strategic_relevance_visible - after_b.strategic_relevance_raw;
        assert!(
            (new_boost - 0.15).abs() < 1e-4,
            "Chosen→Chosen should *reset* to +0.15 (no stacking), got {new_boost}",
        );

        let stances = rt.decision_stances();
        assert_eq!(
            stances.get("tnt"),
            Some(&DecisionStance::Chosen {
                decision_id: dec_b.event_id.clone()
            }),
        );
    }

    #[test]
    fn two_consecutive_dampen_on_same_project_keep_factor_0_3() {
        // Build the dampened runtime: tnt is dampened by Decision A,
        // then again by Decision B. tnt's stance is Dampened{B} and
        // the matching factor stays 0.3.
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("o1", &[])).unwrap();
        rt.ingest(project("o2", &[])).unwrap();
        let _dec_a = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "o1".into(),
                over: vec![],
                dampen: vec!["tnt".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        let dec_b = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "o2".into(),
                over: vec![],
                dampen: vec!["tnt".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();

        let stances = rt.decision_stances();
        assert_eq!(
            stances.get("tnt"),
            Some(&DecisionStance::Dampened {
                decision_id: dec_b.event_id.clone()
            }),
            "Dampened→Dampened: stance.decision_id updates to the latest",
        );

        // Compute the base delta in an undampened runtime.
        let mut base_rt = WorldRuntime::in_memory().unwrap();
        base_rt.ingest(project("tnt", &["ai"])).unwrap();
        base_rt.ingest(signal("ai news", &["ai"], 0.8)).unwrap();
        let base_records = base_rt.advance().unwrap();
        let base_delta = relevance_delta_for(&base_records.records, "tnt");

        rt.ingest(signal("ai news", &["ai"], 0.8)).unwrap();
        let damp_records = rt.advance().unwrap();
        let damp_delta = relevance_delta_for(&damp_records.records, "tnt");
        let ratio = damp_delta / base_delta;
        assert!(
            (ratio - 0.3).abs() < 1e-4,
            "Dampened→Dampened should remain ×0.3, got {ratio}",
        );

        // Cause cites the most-recent (B) decision_id.
        let cited_by_b = damp_records.records.iter().any(|r| {
            r.entity_id == "tnt"
                && r.causes.iter().any(|c| {
                    matches!(
                        c,
                        crate::Cause::DecisionDampened { decision_id, .. }
                            if decision_id == &dec_b.event_id
                    )
                })
        });
        assert!(
            cited_by_b,
            "Cause::DecisionDampened should cite the latest decision_id (B)",
        );
    }

    #[test]
    fn chain_chose_dampen_chose_revoke_yields_expected_state_at_each_step() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("other", &[])).unwrap();

        // 1. chose tnt → Chosen{A} + boost ≈ 0.15.
        let _dec_a = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec![],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        let s1 = rt.inspect_project("tnt").unwrap();
        assert!((s1.strategic_relevance_visible - s1.strategic_relevance_raw - 0.15).abs() < 1e-4);

        // 2. dampen tnt (Decision B chose `other`, dampen tnt) →
        //    Dampened{B}, boost dropped.
        let _dec_b = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "other".into(),
                over: vec![],
                dampen: vec!["tnt".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        let s2 = rt.inspect_project("tnt").unwrap();
        assert!(
            (s2.strategic_relevance_visible - s2.strategic_relevance_raw).abs() < 1e-6,
            "no boost after Chosen→Dampened",
        );

        // 3. chose tnt again (Decision C) → Chosen{C} + fresh boost.
        let dec_c = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec![],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        let s3 = rt.inspect_project("tnt").unwrap();
        assert!(
            (s3.strategic_relevance_visible - s3.strategic_relevance_raw - 0.15).abs() < 1e-4,
            "fresh +0.15 boost on Dampened→Chosen",
        );
        let st3 = rt.decision_stances();
        assert_eq!(
            st3.get("tnt"),
            Some(&DecisionStance::Chosen {
                decision_id: dec_c.event_id.clone()
            }),
        );

        // 4. revoke C → no stance, no boost.
        rt.ingest(WorldEvent::DecisionRevoked {
            decision_id: dec_c.event_id.clone(),
        })
        .unwrap();
        let s4 = rt.inspect_project("tnt").unwrap();
        assert!(
            (s4.strategic_relevance_visible - s4.strategic_relevance_raw).abs() < 1e-6,
            "no boost after revoke",
        );
        let st4 = rt.decision_stances();
        assert!(
            !st4.contains_key("tnt"),
            "no stance after revoke of the last steering Decision: {st4:?}",
        );
    }

    #[test]
    fn supersession_state_is_deterministic_on_repeated_replay() {
        let dir = tempfile_dir();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let dec_c_id;
        {
            let mut rt = WorldRuntime::open_dir(&dir).unwrap();
            rt.ingest_at(project("tnt", &["ai"]), t0).unwrap();
            rt.ingest_at(project("other", &[]), t0).unwrap();
            rt.ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "tnt".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: None,
                },
                t0 + Duration::seconds(1),
            )
            .unwrap();
            rt.ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "other".into(),
                    over: vec![],
                    dampen: vec!["tnt".into()],
                    reason: None,
                    decided_at: None,
                },
                t0 + Duration::seconds(2),
            )
            .unwrap();
            let c = rt
                .ingest_at(
                    WorldEvent::DecisionRecorded {
                        chose: "tnt".into(),
                        over: vec![],
                        dampen: vec![],
                        reason: None,
                        decided_at: None,
                    },
                    t0 + Duration::seconds(3),
                )
                .unwrap();
            dec_c_id = c.event_id;
        }

        // Re-open twice; the derived state must match.
        let mut rt1 = WorldRuntime::open_dir(&dir).unwrap();
        let s1 = rt1.decision_stances();
        let v1 = rt1.inspect_project("tnt").unwrap();
        drop(rt1);

        let mut rt2 = WorldRuntime::open_dir(&dir).unwrap();
        let s2 = rt2.decision_stances();
        let v2 = rt2.inspect_project("tnt").unwrap();

        assert_eq!(s1.get("tnt"), s2.get("tnt"));
        assert_eq!(
            s1.get("tnt"),
            Some(&DecisionStance::Chosen {
                decision_id: dec_c_id
            }),
        );
        assert!(
            (v1.strategic_relevance_raw - v2.strategic_relevance_raw).abs() < 1e-6,
            "raw must be deterministic across replays"
        );
        assert!(
            (v1.strategic_relevance_visible - v2.strategic_relevance_visible).abs() < 1e-6,
            "visible must be deterministic across replays"
        );
    }

    // -------- Decision: decision list polish (issue #7) --------

    use crate::queries::{format_decision_list, SteerKind};

    #[test]
    fn decision_list_single_chose_plus_dampen() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("side-x", &["voice"])).unwrap();
        let dec = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec!["side-x".into()],
                dampen: vec!["side-x".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();

        let list = rt.decision_list().unwrap();
        assert_eq!(list.len(), 1);
        let entry = &list[0];
        assert_eq!(entry.decision_id, dec.event_id);
        assert_eq!(entry.chose, "tnt");
        assert_eq!(entry.over, vec!["side-x".to_string()]);
        assert_eq!(entry.dampen, vec!["side-x".to_string()]);

        // chose-projects first, then dampened. tnt and side-x are
        // different projects → two steers, in that order.
        assert_eq!(entry.steers.len(), 2);
        assert_eq!(entry.steers[0].project_id, "tnt");
        assert!(matches!(entry.steers[0].kind, SteerKind::Chose));
        assert!(entry.steers[0].boost_remaining.unwrap() > 0.14);
        assert_eq!(entry.steers[1].project_id, "side-x");
        assert!(matches!(entry.steers[1].kind, SteerKind::Dampened));
        assert!(entry.steers[1].boost_remaining.is_none());
        assert!(entry.superseded_for.is_empty());
    }

    #[test]
    fn decision_list_chose_wins_when_same_project_in_dampen() {
        // Edge case: chose and dampen both name the same project.
        // Per decision_application_system, chose wins → DecisionStance
        // is Chosen. The list view should show one `Chose` steer,
        // not two.
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        let _dec = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec!["tnt".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();

        let list = rt.decision_list().unwrap();
        assert_eq!(list[0].steers.len(), 1);
        assert!(matches!(list[0].steers[0].kind, SteerKind::Chose));
    }

    #[test]
    fn decision_list_separately_listed_dampen_and_chose() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("side-y", &["voice"])).unwrap();
        let _dec = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec!["side-y".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();

        let list = rt.decision_list().unwrap();
        assert_eq!(list.len(), 1);
        let entry = &list[0];
        assert_eq!(entry.steers.len(), 2);
        assert_eq!(entry.steers[0].project_id, "tnt");
        assert!(matches!(entry.steers[0].kind, SteerKind::Chose));
        assert_eq!(entry.steers[1].project_id, "side-y");
        assert!(matches!(entry.steers[1].kind, SteerKind::Dampened));
    }

    #[test]
    fn decision_list_partial_supersession_emits_superseded_for() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("side-x", &["voice"])).unwrap();
        let dec_a = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec!["side-x".into()],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        let dec_b = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "side-x".into(),
                over: vec![],
                dampen: vec![],
                reason: None,
                decided_at: None,
            })
            .unwrap();

        let list = rt.decision_list().unwrap();
        assert_eq!(list.len(), 2, "two non-revoked decisions");
        let a = list
            .iter()
            .find(|e| e.decision_id == dec_a.event_id)
            .unwrap();
        // A still steers tnt (chose). side-x has been superseded by B.
        assert_eq!(a.steers.len(), 1);
        assert_eq!(a.steers[0].project_id, "tnt");
        assert_eq!(a.superseded_for.len(), 1);
        assert_eq!(a.superseded_for[0].project_id, "side-x");
        assert_eq!(a.superseded_for[0].by_decision_id, dec_b.event_id);

        // B steers side-x; no supersession.
        let b = list
            .iter()
            .find(|e| e.decision_id == dec_b.event_id)
            .unwrap();
        assert_eq!(b.steers.len(), 1);
        assert_eq!(b.steers[0].project_id, "side-x");
        assert!(b.superseded_for.is_empty());
    }

    #[test]
    fn decision_list_excludes_revoked_decisions() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        let dec_a = rt
            .ingest(WorldEvent::DecisionRecorded {
                chose: "tnt".into(),
                over: vec![],
                dampen: vec![],
                reason: None,
                decided_at: None,
            })
            .unwrap();
        rt.ingest(WorldEvent::DecisionRevoked {
            decision_id: dec_a.event_id.clone(),
        })
        .unwrap();
        let list = rt.decision_list().unwrap();
        assert!(
            list.is_empty(),
            "revoked decision should not appear: {list:?}",
        );
    }

    #[test]
    fn decision_list_keeps_zero_boost_decision_if_not_revoked() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        rt.ingest_at(project("tnt", &["ai"]), t0).unwrap();
        let _dec = rt
            .ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "tnt".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: Some(t0),
                },
                t0,
            )
            .unwrap();
        // 5000 event-log days → 0.15 * 0.999^5000 ≈ 0.001.
        rt.ingest_at(
            WorldEvent::TimePulseObserved {
                observed_at: t0 + Duration::days(5000),
            },
            t0 + Duration::days(5000),
        )
        .unwrap();
        let list = rt.decision_list().unwrap();
        assert_eq!(list.len(), 1);
        let boost = list[0].steers[0].boost_remaining.unwrap_or(0.0);
        assert!(
            boost < 0.01,
            "boost should have eroded near zero, got {boost}",
        );
    }

    #[test]
    fn decision_list_chronological_order() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        rt.ingest_at(project("a", &["x"]), t0).unwrap();
        rt.ingest_at(project("b", &["x"]), t0).unwrap();
        let dec1 = rt
            .ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "a".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: None,
                },
                t0 + Duration::seconds(1),
            )
            .unwrap();
        let dec2 = rt
            .ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "b".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: None,
                },
                t0 + Duration::seconds(2),
            )
            .unwrap();

        let list = rt.decision_list().unwrap();
        assert_eq!(list[0].decision_id, dec1.event_id);
        assert_eq!(list[1].decision_id, dec2.event_id);
    }

    #[test]
    fn decision_list_format_matches_spec() {
        // Partial-supersession scenario rendered to the exact format
        // from issue #7. Boost numbers and active-day counts are
        // pulled from the live list so we don't hardcode float math.
        let mut rt = WorldRuntime::in_memory().unwrap();
        let t_a = Utc.with_ymd_and_hms(2026, 5, 19, 0, 0, 0).unwrap();
        let t_b = Utc.with_ymd_and_hms(2026, 5, 21, 0, 0, 0).unwrap();
        rt.ingest_at(project("tnt", &["ai"]), t_a).unwrap();
        rt.ingest_at(project("side-x", &["voice"]), t_a).unwrap();
        let dec_a = rt
            .ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "tnt".into(),
                    over: vec!["side-x".into()],
                    dampen: vec!["side-x".into()],
                    reason: None,
                    decided_at: Some(t_a),
                },
                t_a,
            )
            .unwrap();
        let dec_b = rt
            .ingest_at(
                WorldEvent::DecisionRecorded {
                    chose: "side-x".into(),
                    over: vec![],
                    dampen: vec![],
                    reason: None,
                    decided_at: Some(t_b),
                },
                t_b,
            )
            .unwrap();

        let list = rt.decision_list().unwrap();
        let rendered = format_decision_list(&list);

        let entry_a = list
            .iter()
            .find(|e| e.decision_id == dec_a.event_id)
            .unwrap();
        let entry_b = list
            .iter()
            .find(|e| e.decision_id == dec_b.event_id)
            .unwrap();
        let boost_a = entry_a.steers[0].boost_remaining.unwrap();
        let boost_b = entry_b.steers[0].boost_remaining.unwrap();
        let active_a = entry_a.active_event_log_days;
        let active_b = entry_b.active_event_log_days;

        let expected = format!(
            "{a} chose:tnt over:[side-x] dampen:[side-x]\n  decided 2026-05-19, active {active_a} event-log days\n  steers tnt (chose, boost {ba:.3} remaining of 0.150)\n  superseded for side-x by {b}\n\n{b} chose:side-x over:[] dampen:[]\n  decided 2026-05-21, active {active_b} event-log days\n  steers side-x (chose, boost {bb:.3} remaining of 0.150)\n",
            a = dec_a.event_id,
            b = dec_b.event_id,
            ba = boost_a,
            bb = boost_b,
        );

        assert_eq!(
            rendered, expected,
            "rendered output mismatched spec:\n--- got:\n{rendered}\n--- expected:\n{expected}",
        );
    }

    // -------- Project: depends_on annotation (issue #15) --------
    //
    // Per `docs/CONTEXT.md` `#depends_on` and ADR-0005 (additive variant
    // contract): `Project.depends_on` is a light declarative annotation
    // surfaced in `inspect` only — **no system effects**. Cycles are
    // permitted (rendered flat); unknown project ids are rejected at
    // ingest.

    #[test]
    fn newly_created_project_has_empty_depends_on() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        let view = rt.inspect_project("tnt").expect("project exists");
        assert!(
            view.depends_on.is_empty(),
            "expected empty depends_on by default, got {:?}",
            view.depends_on,
        );
    }

    #[test]
    fn project_update_sets_depends_on_list() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("side-x", &["voice"])).unwrap();
        rt.ingest(WorldEvent::ProjectUpdated {
            id: "tnt".into(),
            name: None,
            tags: None,
            depends_on: Some(vec!["side-x".into()]),
        })
        .unwrap();
        let view = rt.inspect_project("tnt").expect("project exists");
        assert_eq!(view.depends_on, vec!["side-x".to_string()]);
    }

    #[test]
    fn project_update_with_none_depends_on_leaves_field_untouched() {
        // Symmetric with `tags` semantics: `None` ⇒ untouched, `Some(_)`
        // ⇒ full replace.
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai"])).unwrap();
        rt.ingest(project("side-x", &["voice"])).unwrap();
        rt.ingest(WorldEvent::ProjectUpdated {
            id: "tnt".into(),
            name: None,
            tags: None,
            depends_on: Some(vec!["side-x".into()]),
        })
        .unwrap();
        // Now patch the name only — depends_on must persist.
        rt.ingest(WorldEvent::ProjectUpdated {
            id: "tnt".into(),
            name: Some("TNT (voice)".into()),
            tags: None,
            depends_on: None,
        })
        .unwrap();
        let view = rt.inspect_project("tnt").expect("project exists");
        assert_eq!(view.name, "TNT (voice)");
        assert_eq!(
            view.depends_on,
            vec!["side-x".to_string()],
            "depends_on must be untouched when ProjectUpdated.depends_on is None"
        );
    }

    #[test]
    fn project_update_replaces_depends_on_list() {
        // Some(list) is full-replace semantics, mirroring `tags`.
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("a", &[])).unwrap();
        rt.ingest(project("b", &[])).unwrap();
        rt.ingest(project("c", &[])).unwrap();
        rt.ingest(WorldEvent::ProjectUpdated {
            id: "a".into(),
            name: None,
            tags: None,
            depends_on: Some(vec!["b".into()]),
        })
        .unwrap();
        rt.ingest(WorldEvent::ProjectUpdated {
            id: "a".into(),
            name: None,
            tags: None,
            depends_on: Some(vec!["c".into()]),
        })
        .unwrap();
        let view = rt.inspect_project("a").expect("project exists");
        assert_eq!(
            view.depends_on,
            vec!["c".to_string()],
            "the second update should fully replace the first list",
        );
    }

    #[test]
    fn project_update_clears_depends_on_to_empty() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("a", &[])).unwrap();
        rt.ingest(project("b", &[])).unwrap();
        rt.ingest(WorldEvent::ProjectUpdated {
            id: "a".into(),
            name: None,
            tags: None,
            depends_on: Some(vec!["b".into()]),
        })
        .unwrap();
        rt.ingest(WorldEvent::ProjectUpdated {
            id: "a".into(),
            name: None,
            tags: None,
            depends_on: Some(vec![]),
        })
        .unwrap();
        let view = rt.inspect_project("a").expect("project exists");
        assert!(
            view.depends_on.is_empty(),
            "Some(empty) should clear the list, got {:?}",
            view.depends_on
        );
    }

    #[test]
    fn project_update_rejects_unknown_depends_on_id() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("a", &[])).unwrap();
        let initial_count = rt.event_count();
        let result = rt.ingest(WorldEvent::ProjectUpdated {
            id: "a".into(),
            name: None,
            tags: None,
            depends_on: Some(vec!["ghost".into()]),
        });
        assert!(
            matches!(
                result,
                Err(RuntimeError::EntityNotFound {
                    kind: "Project",
                    ..
                })
            ),
            "expected EntityNotFound for unknown depends_on id, got {result:?}"
        );
        assert_eq!(
            rt.event_count(),
            initial_count,
            "rejected event must not be appended to the log",
        );
    }

    #[test]
    fn project_depends_on_allows_cycles() {
        // Per CONTEXT.md `#depends_on`, cycles are permitted: the list is
        // a declarative annotation, not a traversable graph. Validation
        // only checks that ids exist.
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("a", &[])).unwrap();
        rt.ingest(project("b", &[])).unwrap();
        rt.ingest(WorldEvent::ProjectUpdated {
            id: "a".into(),
            name: None,
            tags: None,
            depends_on: Some(vec!["b".into()]),
        })
        .unwrap();
        // The reverse edge — would form a 2-cycle. Must be accepted.
        rt.ingest(WorldEvent::ProjectUpdated {
            id: "b".into(),
            name: None,
            tags: None,
            depends_on: Some(vec!["a".into()]),
        })
        .unwrap();
        let a = rt.inspect_project("a").expect("project a exists");
        let b = rt.inspect_project("b").expect("project b exists");
        assert_eq!(a.depends_on, vec!["b".to_string()]);
        assert_eq!(b.depends_on, vec!["a".to_string()]);
    }

    #[test]
    fn project_depends_on_change_emits_no_change_records() {
        // Property pin: `ProjectUpdated{ depends_on: Some(_) }` must not
        // produce any ChangeRecord — `depends_on` is observed by no
        // system in v1. (resonance / decay / decision-boost are
        // unaffected because they read other fields entirely.)
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("a", &["ai"])).unwrap();
        rt.ingest(project("b", &["voice"])).unwrap();
        // Drain the cursor so any pre-existing records don't bleed in.
        let _ = rt.advance().unwrap();
        rt.ingest(WorldEvent::ProjectUpdated {
            id: "a".into(),
            name: None,
            tags: None,
            depends_on: Some(vec!["b".into()]),
        })
        .unwrap();
        let changes = rt.advance().unwrap();
        assert!(
            changes.is_empty(),
            "depends_on update must produce no ChangeRecords, got {:?}",
            changes.records,
        );
    }

    #[test]
    fn project_depends_on_does_not_perturb_resonance_or_decay() {
        // Stronger version of the above: run an identical event sequence
        // in two runtimes — one with a depends_on update spliced in,
        // one without — and confirm the resonance + decay ChangeRecords
        // come out byte-equivalent on every field that matters.
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let mut baseline = WorldRuntime::in_memory().unwrap();
        baseline
            .ingest_at(project("a", &["ai", "voice"]), t0)
            .unwrap();
        baseline.ingest_at(project("b", &["voice"]), t0).unwrap();
        baseline
            .ingest_at(
                signal("voice progress", &["voice"], 0.7),
                t0 + Duration::minutes(1),
            )
            .unwrap();
        baseline
            .ingest_at(
                WorldEvent::TimePulseObserved {
                    observed_at: t0 + Duration::days(7),
                },
                t0 + Duration::days(7),
            )
            .unwrap();
        let base_changes = baseline.advance().unwrap();

        let mut with_dep = WorldRuntime::in_memory().unwrap();
        with_dep
            .ingest_at(project("a", &["ai", "voice"]), t0)
            .unwrap();
        with_dep.ingest_at(project("b", &["voice"]), t0).unwrap();
        // Splice in the depends_on update *between* projects and the
        // signal. occurred_at matches the signal's predecessor so Now
        // doesn't advance (no decay perturbation from the extra event).
        with_dep
            .ingest_at(
                WorldEvent::ProjectUpdated {
                    id: "a".into(),
                    name: None,
                    tags: None,
                    depends_on: Some(vec!["b".into()]),
                },
                t0,
            )
            .unwrap();
        with_dep
            .ingest_at(
                signal("voice progress", &["voice"], 0.7),
                t0 + Duration::minutes(1),
            )
            .unwrap();
        with_dep
            .ingest_at(
                WorldEvent::TimePulseObserved {
                    observed_at: t0 + Duration::days(7),
                },
                t0 + Duration::days(7),
            )
            .unwrap();
        let with_changes = with_dep.advance().unwrap();

        assert_eq!(
            base_changes.records.len(),
            with_changes.records.len(),
            "record counts differ:\nbase: {:?}\nwith: {:?}",
            base_changes.records,
            with_changes.records,
        );
        for (x, y) in base_changes.records.iter().zip(with_changes.records.iter()) {
            assert_eq!(x.entity_id, y.entity_id, "entity_id drift");
            assert_eq!(x.field, y.field, "field drift");
            assert!(
                (x.before - y.before).abs() < 1e-6,
                "before drift: {} vs {}",
                x.before,
                y.before,
            );
            assert!(
                (x.after - y.after).abs() < 1e-6,
                "after drift: {} vs {}",
                x.after,
                y.after,
            );
        }
        // And the depends_on annotation should be present on `a` after
        // replay, untouched by matching/decay.
        let a = with_dep.inspect_project("a").unwrap();
        assert_eq!(a.depends_on, vec!["b".to_string()]);
    }

    #[test]
    fn project_depends_on_persists_through_open_dir_replay() {
        // JSONL round-trip: write a depends_on update, re-open, confirm
        // the field survives replay. Also confirms the event-log layer
        // serializes / deserializes the new optional payload field.
        let dir = tempfile_dir();
        {
            let mut rt = WorldRuntime::open_dir(&dir).unwrap();
            rt.ingest(project("a", &["ai"])).unwrap();
            rt.ingest(project("b", &["voice"])).unwrap();
            rt.ingest(WorldEvent::ProjectUpdated {
                id: "a".into(),
                name: None,
                tags: None,
                depends_on: Some(vec!["b".into()]),
            })
            .unwrap();
        }
        let mut rt = WorldRuntime::open_dir(&dir).unwrap();
        let a = rt.inspect_project("a").expect("project a persisted");
        assert_eq!(a.depends_on, vec!["b".to_string()]);
    }

    #[test]
    fn project_depends_on_legacy_jsonl_without_field_loads_cleanly() {
        // Backward-compat (ADR-0005 option-B): a hand-written
        // ProjectUpdated row that omits `depends_on` entirely must
        // deserialize as if `depends_on: None` were present.
        use std::io::Write as _;
        let dir = tempfile_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let events_path = dir.join("events.jsonl");
        // Two ProjectCreated rows + one ProjectUpdated row missing the
        // new field. These ids are ULIDs by length; the runtime tolerates
        // hand-written ids on read (the EventId ordering uses string
        // compare and ULIDs are lexicographically sortable, but for the
        // purposes of this test we only check that load doesn't panic).
        // Lines are written via `write_all` to avoid clippy's
        // `write_literal` lint on `writeln!("{}", literal)`.
        let mut f = std::fs::File::create(&events_path).unwrap();
        let rows = [
            r#"{"id":"01AAAAAAAAAAAAAAAAAAAAAAAA","occurred_at":"2026-01-01T00:00:00Z","payload":{"kind":"ProjectCreated","id":"a","name":"A","tags":["ai"]}}"#,
            r#"{"id":"01AAAAAAAAAAAAAAAAAAAAAAAB","occurred_at":"2026-01-01T00:00:01Z","payload":{"kind":"ProjectCreated","id":"b","name":"B","tags":["voice"]}}"#,
            // No `depends_on` field on this update — additive variant test.
            r#"{"id":"01AAAAAAAAAAAAAAAAAAAAAAAC","occurred_at":"2026-01-01T00:00:02Z","payload":{"kind":"ProjectUpdated","id":"a","name":"A renamed"}}"#,
        ];
        for row in rows {
            f.write_all(row.as_bytes()).unwrap();
            f.write_all(b"\n").unwrap();
        }
        drop(f);

        let mut rt = WorldRuntime::open_dir(&dir).unwrap();
        let a = rt.inspect_project("a").expect("project a persisted");
        assert_eq!(a.name, "A renamed");
        assert!(
            a.depends_on.is_empty(),
            "old log entry without depends_on must deserialize as empty, got {:?}",
            a.depends_on
        );
    }

    // -------- Explain: full causal history (issue #16) --------
    //
    // `explain_project_history` returns every ChangeRecord whose
    // `entity_id` matches the target project, in event-log order
    // (CONTEXT.md `#explanation`). ChangeRecord-only — the
    // `ProjectCreated` event is **not** part of the chain. Empty list
    // for an untouched project; loud error for an unknown id.

    #[test]
    fn explain_project_history_empty_for_untouched_project() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("alone", &["ai"])).unwrap();
        let explanation = rt.explain_project_history("alone").expect("project exists");
        assert!(
            explanation.records.is_empty(),
            "untouched project should produce no history records, got {:?}",
            explanation.records,
        );
    }

    #[test]
    fn explain_project_history_excludes_creation_event() {
        // Pin the spec: ProjectCreated is NOT a ChangeRecord (only
        // system-derived effects are). The single ChangeRecord here is
        // the signal-driven relevance bump, not the project spawn.
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("tnt", &["ai", "voice"])).unwrap();
        rt.ingest(signal("voice progress", &["voice", "ai"], 0.6))
            .unwrap();
        let explanation = rt.explain_project_history("tnt").unwrap();
        // Two records (relevance + urgency from the signal match) — no
        // creation entry, no extra phantom records.
        assert!(
            !explanation.records.is_empty(),
            "signal match should yield ChangeRecords",
        );
        for r in &explanation.records {
            assert!(
                matches!(r.field.as_str(), "strategic_relevance" | "urgency"),
                "history must contain only system-derived fields, got {}",
                r.field
            );
        }
    }

    #[test]
    fn explain_project_history_orders_records_chronologically_across_advances() {
        // Multiple advance windows with decay between signals — confirm
        // every record makes it into the history, in event-log order.
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest_at(project("p", &["ai", "voice"]), t0).unwrap();
        rt.ingest_at(
            signal("voice news", &["voice"], 0.7),
            t0 + Duration::minutes(1),
        )
        .unwrap();
        rt.advance().unwrap();
        rt.ingest_at(
            WorldEvent::TimePulseObserved {
                observed_at: t0 + Duration::days(30),
            },
            t0 + Duration::days(30),
        )
        .unwrap();
        rt.advance().unwrap();
        rt.ingest_at(
            signal("more voice news", &["voice"], 0.6),
            t0 + Duration::days(31),
        )
        .unwrap();
        rt.advance().unwrap();

        let explanation = rt.explain_project_history("p").unwrap();
        assert!(
            explanation.records.len() >= 3,
            "expected ≥3 records (initial match + decay + new match), got {}",
            explanation.records.len(),
        );
        // Triggering event ids are sorted ascending (ULIDs are
        // lexicographically chronological).
        for w in explanation.records.windows(2) {
            assert!(
                w[0].triggered_by_event <= w[1].triggered_by_event,
                "records out of event-log order: {:?} > {:?}",
                w[0].triggered_by_event,
                w[1].triggered_by_event,
            );
        }
    }

    #[test]
    fn explain_project_history_filters_by_entity_id() {
        // Two projects, signals to each — history for one returns only
        // its records, no cross-talk.
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("a", &["ai"])).unwrap();
        rt.ingest(project("b", &["voice"])).unwrap();
        rt.ingest(signal("ai news", &["ai"], 0.6)).unwrap();
        rt.ingest(signal("voice news", &["voice"], 0.6)).unwrap();

        let history_a = rt.explain_project_history("a").unwrap();
        assert!(!history_a.records.is_empty(), "a should have records");
        for r in &history_a.records {
            assert_eq!(
                r.entity_id, "a",
                "history for 'a' must not leak 'b' records"
            );
        }
        let history_b = rt.explain_project_history("b").unwrap();
        assert!(!history_b.records.is_empty(), "b should have records");
        for r in &history_b.records {
            assert_eq!(
                r.entity_id, "b",
                "history for 'b' must not leak 'a' records"
            );
        }
    }

    #[test]
    fn explain_project_history_returns_error_for_unknown_id() {
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("real", &["ai"])).unwrap();
        let result = rt.explain_project_history("ghost");
        assert!(
            matches!(
                result,
                Err(RuntimeError::EntityNotFound {
                    kind: "Project",
                    ..
                })
            ),
            "unknown id should be loud, got {result:?}",
        );
    }

    #[test]
    fn explain_project_history_performance_under_1k_events() {
        // Acceptance #6: ~1,000 events, --all completes well under 1s.
        // Run the query on the busiest project and measure wall-clock
        // time around the call only (build-up time excluded).
        let mut rt = WorldRuntime::in_memory().unwrap();
        rt.ingest(project("hot", &["ai", "voice"])).unwrap();
        rt.ingest(project("cold", &["unrelated"])).unwrap();
        // 1,000 - 2 = 998 signals; the matching system emits ≥1 record
        // per matching signal so the resulting ChangeLog is large.
        for i in 0..998 {
            let tag = if i % 2 == 0 { "ai" } else { "voice" };
            rt.ingest(signal(&format!("s{i}"), &[tag], 0.4)).unwrap();
        }
        let started = std::time::Instant::now();
        let history = rt
            .explain_project_history("hot")
            .expect("hot project exists");
        let elapsed = started.elapsed();
        assert!(
            !history.records.is_empty(),
            "expected history records, log was non-trivial",
        );
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "explain_project_history over ~1k events should run \
             well under 1s, took {elapsed:?}",
        );
    }
}

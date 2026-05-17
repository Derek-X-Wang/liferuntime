use bevy_ecs::prelude::*;
use chrono::{DateTime, Utc};
use liferuntime_event_log::{
    EventId, EventLog, EventRange, JsonlEventLog, MemoryEventLog, StoredEvent,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::errors::RuntimeError;
use crate::events::WorldEvent;
use crate::explanation::{ChangeLog, ChangeRecord, ExplainTarget, Explanation};
use crate::model::{
    Goal, GoalStatus, Identity, LastTouched, LatestEventId, Now, Project, ProjectStatus, Signal,
    Unprocessed,
};
use crate::queries::ProjectView;
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
                l.append(stored.clone()).unwrap_or_else(|never| match never {});
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

    fn with_backend(
        log: EventLogBackend,
        dir: Option<PathBuf>,
    ) -> Result<Self, RuntimeError> {
        let mut world = World::new();
        world.init_resource::<ChangeLog>();
        world.init_resource::<Now>();
        world.init_resource::<LatestEventId>();

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
            .filter(|r| {
                cursor_id
                    .as_ref()
                    .is_none_or(|c| r.triggered_by_event > *c)
            })
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

    pub fn inspect_project(&mut self, id: &str) -> Option<ProjectView> {
        let mut q = self.world.query::<(&Identity, &Project)>();
        for (ident, project) in q.iter(&self.world) {
            if ident.0 == id {
                return Some(ProjectView {
                    id: ident.0.clone(),
                    name: project.name.clone(),
                    tags: project.tags.clone(),
                    strategic_relevance: project.strategic_relevance,
                    urgency: project.urgency,
                    status: project.status,
                    archived_reason: project.archived_reason.clone(),
                    completion_note: project.completion_note.clone(),
                });
            }
        }
        None
    }

    pub fn explain(&self, target: ExplainTarget) -> Result<Explanation, RuntimeError> {
        let records = self.world.resource::<ChangeLog>().records.clone();
        let scoped: Vec<ChangeRecord> = match target {
            ExplainTarget::LatestChange => records,
            ExplainTarget::Entity(id) => records
                .into_iter()
                .filter(|r| r.entity_id == id)
                .collect(),
        };
        Ok(Explanation { records: scoped })
    }

    pub fn event_count(&self) -> usize {
        self.log.replay_all().map(|v| v.len()).unwrap_or(0)
    }
}

fn apply_event(world: &mut World, stored: &StoredEvent<WorldEvent>) {
    // Maintain time-tracking resources first so any system reading them
    // sees the new event's clock position.
    {
        let mut now = world.resource_mut::<Now>();
        if stored.occurred_at > now.at() {
            *now = Now(stored.occurred_at);
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
            world.spawn((
                Identity(id.clone()),
                Project::new(name.clone(), tags.clone()),
                LastTouched {
                    at: stored.occurred_at,
                },
            ));
        }
        WorldEvent::ProjectUpdated { id, name, tags } => {
            let mut q = world.query::<(&Identity, &mut Project)>();
            for (ident, mut project) in q.iter_mut(world) {
                if ident.0 == *id {
                    if let Some(n) = name {
                        project.name = n.clone();
                    }
                    if let Some(t) = tags {
                        project.tags = t.clone();
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
        assert!(rendered.contains("tnt"), "explanation missing entity: {rendered}");
        assert!(rendered.contains("voice"), "explanation missing tag: {rendered}");
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
            (signal("voice models improving", &["ai", "voice"], 0.7), t0 + Duration::hours(1)),
            (signal("ai funding wave", &["ai"], 0.5), t0 + Duration::hours(2)),
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
        runtime
            .ingest(project("tnt", &["ai", "voice"]))
            .unwrap();
        runtime
            .ingest(signal("random news", &["politics"], 0.9))
            .unwrap();
        let changes = runtime.advance().unwrap();
        assert!(changes.is_empty(), "no project should match: {:?}", changes.records);
    }

    #[test]
    fn second_advance_with_no_new_events_is_empty() {
        let mut runtime = WorldRuntime::in_memory().unwrap();
        runtime
            .ingest(project("p1", &["ai"]))
            .unwrap();
        runtime
            .ingest(signal("ai stuff", &["ai"], 0.5))
            .unwrap();
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
        runtime
            .ingest(project("p1", &["ai", "voice"]))
            .unwrap();
        runtime
            .ingest(signal("voice models", &["voice", "ai"], 0.9))
            .unwrap();
        // Under per-event scheduling (ADR-0006), state is already
        // derived — no explicit materialize step needed.
        let view = runtime.inspect_project("p1").expect("project exists");
        assert!(
            view.strategic_relevance > 0.5,
            "expected strategic_relevance > 0.5 after matching signal, got {}",
            view.strategic_relevance
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
        assert!((va.strategic_relevance - vb.strategic_relevance).abs() < 1e-6);
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
            view.strategic_relevance > 0.5,
            "signal arrived before archive — relevance should be > 0.5, got {}",
            view.strategic_relevance
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
            view.strategic_relevance > 0.5,
            "signal that arrived before archive should still have bumped relevance, got {}",
            view.strategic_relevance
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
            (view.strategic_relevance - 0.5).abs() < 1e-6,
            "archived project should remain at default 0.5, got {}",
            view.strategic_relevance
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
        assert!(rendered, "amplified change should carry a GoalAmplified cause");
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
            rt.ingest_with_key(
                signal("ai news", &["ai"], 0.6),
                Some("kron-job-42".into()),
            )
            .unwrap();
        }

        // Second "CLI invocation" with the same key should be a no-op.
        let mut rt = WorldRuntime::open_dir(&dir).unwrap();
        let before_advance = rt.event_count();
        rt.ingest_with_key(
            signal("ai news", &["ai"], 0.6),
            Some("kron-job-42".into()),
        )
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
        runtime
            .ingest(project("tnt", &["ai", "voice"]))
            .unwrap();
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

        runtime
            .ingest_at(project("p1", &["ai"]), t0)
            .unwrap();
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
}

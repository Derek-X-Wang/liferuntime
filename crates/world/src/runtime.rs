use bevy_ecs::prelude::*;
use liferuntime_event_log::{
    EventId, EventLog, EventRange, JsonlEventLog, MemoryEventLog, StoredEvent,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::errors::RuntimeError;
use crate::events::WorldEvent;
use crate::explanation::{ChangeLog, ChangeRecord, ExplainTarget, Explanation};
use crate::model::{Goal, Identity, Project, Signal, Unprocessed};
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

/// The deterministic core of LifeRuntime.
///
/// One method per verb: `ingest` writes an event, `advance` runs systems and
/// returns the delta, `materialize` runs systems without recording a delta
/// (used for read-only queries that need derived state), `inspect_project`
/// reads a projected view, `explain` renders the last-recorded changes.
pub struct WorldRuntime {
    world: World,
    schedule: Schedule,
    log: EventLogBackend,
    cursor: Cursor,
    dir: Option<PathBuf>,
}

enum EventLogBackend {
    Memory(MemoryEventLog<WorldEvent>),
    Jsonl(JsonlEventLog<WorldEvent>),
}

impl EventLogBackend {
    fn append(&mut self, payload: WorldEvent) -> Result<StoredEvent<WorldEvent>, RuntimeError> {
        let stored = StoredEvent::new(payload);
        match self {
            Self::Memory(l) => {
                // Infallible: unwrap is the unambiguous way to discard the never-typed error.
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

    /// Open a runtime rooted at `dir`. Creates the directory and the
    /// `events.jsonl` file if they do not exist, replays all persisted
    /// events, and loads the advance cursor.
    pub fn open_dir(dir: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let log = JsonlEventLog::open(dir.join("events.jsonl"))?;
        let cursor = load_cursor(&dir.join("cursor.json"))?;
        let mut rt = Self::with_backend(EventLogBackend::Jsonl(log), Some(dir))?;
        rt.cursor = cursor;
        rt.replay()?;
        Ok(rt)
    }

    fn with_backend(
        log: EventLogBackend,
        dir: Option<PathBuf>,
    ) -> Result<Self, RuntimeError> {
        let mut world = World::new();
        world.init_resource::<ChangeLog>();

        let mut schedule = Schedule::default();
        register_systems(&mut schedule);

        Ok(Self {
            world,
            schedule,
            log,
            cursor: Cursor::default(),
            dir,
        })
    }

    fn replay(&mut self) -> Result<(), RuntimeError> {
        let events = self.log.replay_all()?;
        for stored in events {
            apply_event(&mut self.world, &stored);
        }
        Ok(())
    }

    pub fn ingest(&mut self, event: WorldEvent) -> Result<IngestReceipt, RuntimeError> {
        let stored = self.log.append(event)?;
        apply_event(&mut self.world, &stored);
        Ok(IngestReceipt {
            event_id: stored.id,
        })
    }

    /// Derive current state without recording an "advance" cursor jump. Use
    /// from read-only commands (`inspect`) that need state to reflect all
    /// observed signals.
    pub fn materialize(&mut self) -> Result<(), RuntimeError> {
        self.schedule.run(&mut self.world);
        Ok(())
    }

    /// Run the schedule, emit the delta of changes triggered by events past
    /// the last cursor, advance the cursor, persist it.
    pub fn advance(&mut self) -> Result<WorldChanges, RuntimeError> {
        self.world.resource_mut::<ChangeLog>().records.clear();
        self.schedule.run(&mut self.world);

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
                    momentum: project.momentum,
                    maintenance_burden: project.maintenance_burden,
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
    match &stored.payload {
        WorldEvent::ProjectCreated { id, name, tags } => {
            world.spawn((
                Identity(id.clone()),
                Project::new(name.clone(), tags.clone()),
            ));
        }
        WorldEvent::GoalCreated {
            id,
            name,
            tags,
            importance,
        } => {
            world.spawn((
                Identity(id.clone()),
                Goal {
                    name: name.clone(),
                    tags: tags.clone(),
                    importance: *importance,
                },
            ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::WorldEvent;

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
        let events = vec![
            project("p1", &["ai", "voice"]),
            signal("voice models improving", &["ai", "voice"], 0.7),
            signal("ai funding wave", &["ai"], 0.5),
        ];

        let mut a = WorldRuntime::in_memory().unwrap();
        let mut b = WorldRuntime::in_memory().unwrap();
        for e in &events {
            a.ingest(e.clone()).unwrap();
            b.ingest(e.clone()).unwrap();
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
        runtime.materialize().unwrap();
        let view = runtime.inspect_project("p1").expect("project exists");
        assert!(
            view.strategic_relevance > 0.5,
            "expected strategic_relevance > 0.5 after matching signal, got {}",
            view.strategic_relevance
        );
    }
}


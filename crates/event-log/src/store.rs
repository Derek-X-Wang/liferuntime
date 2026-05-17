use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::fmt;

/// Stable, lexicographically-sortable identifier for an event.
///
/// Backed by a ULID. We use [`ulid::Generator`] (thread-local) instead of
/// the default `Ulid::new()` so two ids minted in the same millisecond are
/// guaranteed monotonic — otherwise the cursor-based delta filter in
/// `WorldRuntime::advance` can drop derived records when their triggering
/// event id sorts before the cursor by accident.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(pub String);

thread_local! {
    static GENERATOR: RefCell<ulid::Generator> = const { RefCell::new(ulid::Generator::new()) };
}

impl EventId {
    pub fn new() -> Self {
        let ulid = GENERATOR.with(|g| {
            g.borrow_mut()
                .generate()
                .expect("ulid generator should not overflow within one millisecond")
        });
        Self(ulid.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

/// A payload wrapped with provenance — id, timestamp, and an optional
/// idempotency key — for storage.
///
/// `idempotency_key` is None for events ingested without a key (e.g.,
/// interactive CLI use). When present, the runtime uses it to dedupe
/// retries: a second ingest with the same key is a no-op and returns
/// the existing event id.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredEvent<T> {
    pub id: EventId,
    pub occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    pub payload: T,
}

impl<T> StoredEvent<T> {
    pub fn new(payload: T) -> Self {
        Self {
            id: EventId::new(),
            occurred_at: Utc::now(),
            idempotency_key: None,
            payload,
        }
    }

    pub fn with_idempotency_key(payload: T, key: String) -> Self {
        Self {
            id: EventId::new(),
            occurred_at: Utc::now(),
            idempotency_key: Some(key),
            payload,
        }
    }
}

#[derive(Clone, Debug)]
pub enum EventRange {
    /// Every event ever appended, in insertion order.
    All,
    /// Events strictly after the given id (lexicographic on the ULID).
    After(EventId),
}

/// Append-only persistence interface for events.
///
/// Implementations must guarantee:
/// 1. `append` returns the id of the just-stored event.
/// 2. `replay(All)` returns events in insertion order.
/// 3. The same sequence of `append` calls produces the same observable replay.
///
/// Implementations do *not* need to provide read-your-writes across processes
/// unless documented (the JSONL adapter does; in-memory obviously does too
/// within a single process).
pub trait EventLog<T>
where
    T: Clone + Serialize + serde::de::DeserializeOwned,
{
    type Error: std::error::Error + Send + Sync + 'static;

    fn append(&mut self, event: StoredEvent<T>) -> Result<EventId, Self::Error>;
    fn replay(&self, range: EventRange) -> Result<Vec<StoredEvent<T>>, Self::Error>;
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

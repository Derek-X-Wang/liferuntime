use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable, lexicographically-sortable identifier for an event.
///
/// Backed by a ULID so identifiers are monotonic over time and comparable
/// as strings without parsing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(pub String);

impl EventId {
    pub fn new() -> Self {
        Self(ulid::Ulid::new().to_string())
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

/// A payload wrapped with provenance — id and timestamp — for storage.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredEvent<T> {
    pub id: EventId,
    pub occurred_at: DateTime<Utc>,
    pub payload: T,
}

impl<T> StoredEvent<T> {
    pub fn new(payload: T) -> Self {
        Self {
            id: EventId::new(),
            occurred_at: Utc::now(),
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

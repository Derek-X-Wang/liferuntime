use super::store::{EventId, EventLog, EventRange, StoredEvent};
use serde::{de::DeserializeOwned, Serialize};
use std::convert::Infallible;

/// In-memory event log for tests and ephemeral runtimes.
pub struct MemoryEventLog<T> {
    events: Vec<StoredEvent<T>>,
}

impl<T> MemoryEventLog<T> {
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }
}

impl<T> Default for MemoryEventLog<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> EventLog<T> for MemoryEventLog<T>
where
    T: Clone + Serialize + DeserializeOwned,
{
    type Error = Infallible;

    fn append(&mut self, event: StoredEvent<T>) -> Result<EventId, Self::Error> {
        let id = event.id.clone();
        self.events.push(event);
        Ok(id)
    }

    fn replay(&self, range: EventRange) -> Result<Vec<StoredEvent<T>>, Self::Error> {
        let out = match range {
            EventRange::All => self.events.clone(),
            EventRange::After(after) => self
                .events
                .iter()
                .filter(|e| e.id > after)
                .cloned()
                .collect(),
        };
        Ok(out)
    }

    fn len(&self) -> usize {
        self.events.len()
    }
}

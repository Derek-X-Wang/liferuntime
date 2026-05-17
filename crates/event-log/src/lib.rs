//! Append-only event log for the LifeRuntime world.
//!
//! This crate is a deep module: callers append events and replay them, and
//! the storage adapter is chosen at construction time. The trait
//! [`EventLog`] is the public seam; production code uses [`JsonlEventLog`]
//! and tests use [`MemoryEventLog`].
//!
//! The log is intentionally *not* tied to the `world` crate's event types.
//! It is generic over a payload `T: Serialize + DeserializeOwned`, so the
//! event log can be reused for other event-sourced subsystems if they emerge.

mod jsonl;
mod memory;
mod store;

pub use jsonl::{JsonlError, JsonlEventLog, SCHEMA_VERSION};
pub use memory::MemoryEventLog;
pub use store::{EventId, EventLog, EventRange, StoredEvent};

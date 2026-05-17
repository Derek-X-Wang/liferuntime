use super::store::{EventId, EventLog, EventRange, StoredEvent};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Schema version of the on-disk JSONL log written by this binary.
///
/// Bumped only when a breaking change to `WorldEvent` (rename, remove, or
/// payload reshape) lands. Adding new variants does not require a bump.
/// See ADR-0005 for the compatibility contract.
pub const SCHEMA_VERSION: u32 = 1;

const SCHEMA_MARKER: &str = "liferuntime-schema";

#[derive(Debug, Error)]
pub enum JsonlError {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("malformed JSON in event log line {line}: {source}")]
    Json {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("event log schema version {found} is newer than this binary's expected version {expected} — upgrade liferuntime or run `liferuntime migrate`")]
    SchemaTooNew { found: u32, expected: u32 },
}

/// File-backed append-only event log.
///
/// One JSON object per line. The first line is a metadata header
/// recording the schema version; subsequent lines are events. Old logs
/// (pre-header) are treated as version 1.
pub struct JsonlEventLog<T> {
    path: PathBuf,
    cache: Vec<StoredEvent<T>>,
}

#[derive(Serialize, Deserialize)]
struct SchemaHeader {
    #[serde(rename = "_meta")]
    meta: String,
    version: u32,
}

impl<T> JsonlEventLog<T>
where
    T: Clone + Serialize + DeserializeOwned,
{
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JsonlError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| JsonlError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let was_empty = !path.exists() || file_is_empty(&path)?;
        if !path.exists() {
            File::create(&path).map_err(|source| JsonlError::Io {
                path: path.clone(),
                source,
            })?;
        }

        if was_empty {
            // Fresh log: write the schema header so future readers know
            // exactly what produced it.
            write_header(&path)?;
        }

        let file = File::open(&path).map_err(|source| JsonlError::Io {
            path: path.clone(),
            source,
        })?;
        let mut cache = Vec::new();
        for (idx, raw) in BufReader::new(file).lines().enumerate() {
            let raw = raw.map_err(|source| JsonlError::Io {
                path: path.clone(),
                source,
            })?;
            if raw.trim().is_empty() {
                continue;
            }
            // The first non-empty line may be a schema header. If so,
            // validate and skip; if not, this is a pre-header log
            // (implicitly version 1).
            if idx == 0 {
                if let Ok(header) = serde_json::from_str::<SchemaHeader>(&raw) {
                    if header.meta == SCHEMA_MARKER {
                        if header.version > SCHEMA_VERSION {
                            return Err(JsonlError::SchemaTooNew {
                                found: header.version,
                                expected: SCHEMA_VERSION,
                            });
                        }
                        continue;
                    }
                }
                // First line isn't a header → fall through, treat as
                // a normal event in a pre-header log.
            }
            let event: StoredEvent<T> =
                serde_json::from_str(&raw).map_err(|source| JsonlError::Json {
                    line: idx + 1,
                    source,
                })?;
            cache.push(event);
        }
        Ok(Self { path, cache })
    }
}

fn file_is_empty(path: &Path) -> Result<bool, JsonlError> {
    let meta = std::fs::metadata(path).map_err(|source| JsonlError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(meta.len() == 0)
}

fn write_header(path: &Path) -> Result<(), JsonlError> {
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|source| JsonlError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let header = SchemaHeader {
        meta: SCHEMA_MARKER.into(),
        version: SCHEMA_VERSION,
    };
    let line = serde_json::to_string(&header).map_err(|source| JsonlError::Json {
        line: 1,
        source,
    })?;
    writeln!(file, "{line}").map_err(|source| JsonlError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

impl<T> EventLog<T> for JsonlEventLog<T>
where
    T: Clone + Serialize + DeserializeOwned,
{
    type Error = JsonlError;

    fn append(&mut self, event: StoredEvent<T>) -> Result<EventId, Self::Error> {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|source| JsonlError::Io {
                path: self.path.clone(),
                source,
            })?;
        let line = serde_json::to_string(&event).map_err(|source| JsonlError::Json {
            line: self.cache.len() + 2, // +1 for header, +1 for next line
            source,
        })?;
        writeln!(file, "{line}").map_err(|source| JsonlError::Io {
            path: self.path.clone(),
            source,
        })?;
        let id = event.id.clone();
        self.cache.push(event);
        Ok(id)
    }

    fn replay(&self, range: EventRange) -> Result<Vec<StoredEvent<T>>, Self::Error> {
        let out = match range {
            EventRange::All => self.cache.clone(),
            EventRange::After(after) => self
                .cache
                .iter()
                .filter(|e| e.id > after)
                .cloned()
                .collect(),
        };
        Ok(out)
    }

    fn len(&self) -> usize {
        self.cache.len()
    }
}

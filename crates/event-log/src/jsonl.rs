use super::store::{EventId, EventLog, EventRange, StoredEvent};
use serde::{de::DeserializeOwned, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

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
}

/// File-backed append-only event log.
///
/// One JSON object per line. Files are opened in append mode for writes; the
/// in-memory cache is the canonical replay source within the process.
pub struct JsonlEventLog<T> {
    path: PathBuf,
    cache: Vec<StoredEvent<T>>,
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
        if !path.exists() {
            File::create(&path).map_err(|source| JsonlError::Io {
                path: path.clone(),
                source,
            })?;
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
            line: self.cache.len() + 1,
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

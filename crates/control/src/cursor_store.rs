//! Disk-backed persistence of the agent's applied-command
//! cursor.
//!
//! The cursor survives agent restarts so that, on reconnect, the
//! controller can resend only commands newer than the last one the
//! agent applied. Without this the controller either has to replay
//! everything (expensive) or the agent starts missing commands.
//!
//! File format: small JSON blob with `{last_applied, updated_at_ms}`
//! written atomically via tempfile + rename. Failure to persist
//! is logged but non-fatal — an agent that can't write its
//! cursor still quotes correctly; it just takes a full resume
//! on next reconnect.

use std::path::{Path, PathBuf};

use crate::seq::Cursor;

#[derive(Debug, thiserror::Error)]
pub enum CursorStoreError {
    #[error("io error on cursor store: {0}")]
    Io(#[from] std::io::Error),
    #[error("cursor file is corrupt or malformed: {0}")]
    Parse(String),
}

/// File-backed cursor store. Cheap to clone (holds just a path).
#[derive(Debug, Clone)]
pub struct FileCursorStore {
    path: PathBuf,
}

impl FileCursorStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the cursor if a file exists; return `None` when the
    /// file is absent (first boot).
    pub fn load(&self) -> Result<Option<Cursor>, CursorStoreError> {
        match std::fs::read_to_string(&self.path) {
            Ok(body) => {
                let c: Cursor = serde_json::from_str(&body)
                    .map_err(|e| CursorStoreError::Parse(e.to_string()))?;
                Ok(Some(c))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CursorStoreError::Io(e)),
        }
    }

    /// Write the cursor atomically: write to `{path}.tmp`, fsync
    /// sibling, rename into place. Best-effort — the tempfile is
    /// leaked on rename failure rather than rolled back because
    /// rollback would require deleting a possibly-partial file
    /// the operator needs for debugging.
    pub fn save(&self, cursor: &Cursor) -> Result<(), CursorStoreError> {
        let body =
            serde_json::to_string(cursor).map_err(|e| CursorStoreError::Parse(e.to_string()))?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seq::Seq;

    #[test]
    fn save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileCursorStore::new(tmp.path().join("cursor.json"));
        let mut c = Cursor::fresh();
        c.advance(Seq(42));
        store.save(&c).unwrap();
        let loaded = store.load().unwrap().expect("cursor present");
        assert_eq!(loaded.last_applied, Seq(42));
    }

    #[test]
    fn missing_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileCursorStore::new(tmp.path().join("absent.json"));
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn corrupt_file_is_typed_error() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("cursor.json");
        std::fs::write(&p, b"{ not valid json").unwrap();
        let store = FileCursorStore::new(p);
        assert!(matches!(store.load(), Err(CursorStoreError::Parse(_))));
    }
}

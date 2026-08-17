// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! `SQLite` last-N conversation memory (session = Slack channel id).

use crate::sqlite::open_configured;
use chrono::Local;
use rusqlite::{params, Connection};
use std::path::Path;
use thiserror::Error;

const MESSAGES_SCHEMA_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/messages_schema.sql"
));
const MESSAGES_APPEND_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/messages_append.sql"
));
const MESSAGES_GET_LAST_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/messages_get_last.sql"
));

/// One stored turn: role (`user` / `assistant`) and text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
}

/// SQLite-backed message store.
pub struct MemoryDb {
    conn: Connection,
}

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("open memory db: {0}")]
    Open(#[from] rusqlite::Error),
    #[error("memory: {0}")]
    Other(String),
}

impl MemoryDb {
    /// Opens (or creates) the DB and ensures the messages schema exists.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Open`] on `SQLite` open failure, or [`MemoryError::Other`] for query failures.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    MemoryError::Other(format!("create parent {}: {e}", parent.display()))
                })?;
            }
        }
        let conn = open_configured(path.as_ref())?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    fn init(&self) -> Result<(), MemoryError> {
        self.conn
            .execute_batch(MESSAGES_SCHEMA_SQL)
            .map_err(|e| MemoryError::Other(format!("schema: {e}")))?;
        Ok(())
    }

    /// Appends a non-empty message for `session_id`.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Open`] on `SQLite` open failure, or [`MemoryError::Other`] for query failures.
    pub fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<(), MemoryError> {
        if content.trim().is_empty() {
            return Ok(());
        }
        let now = Local::now().to_rfc3339();
        self.conn
            .execute(MESSAGES_APPEND_SQL, params![session_id, role, content, now])
            .map_err(|e| MemoryError::Other(format!("append: {e}")))?;
        Ok(())
    }

    /// Returns the last `n` messages oldest-first.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Open`] on `SQLite` open failure, or [`MemoryError::Other`] for query failures.
    pub fn get_last_messages(
        &self,
        session_id: &str,
        n: u32,
    ) -> Result<Vec<StoredMessage>, MemoryError> {
        let mut stmt = self
            .conn
            .prepare(MESSAGES_GET_LAST_SQL)
            .map_err(|e| MemoryError::Other(format!("prepare get_last: {e}")))?;
        let rows = stmt
            .query_map(params![session_id, i64::from(n)], |row| {
                Ok(StoredMessage {
                    role: row.get(0)?,
                    content: row.get(1)?,
                })
            })
            .map_err(|e| MemoryError::Other(format!("query get_last: {e}")))?;
        let mut out: Vec<StoredMessage> = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| MemoryError::Other(format!("row get_last: {e}")))?;
        out.reverse();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn append_and_get_last_n_oldest_first() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("runtime.db");
        let db = MemoryDb::open(&path).expect("open");
        db.append_message("C1", "user", "one").expect("a1");
        db.append_message("C1", "assistant", "two").expect("a2");
        db.append_message("C1", "user", "three").expect("a3");
        let got = db.get_last_messages("C1", 2).expect("get");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].role, "assistant");
        assert_eq!(got[0].content, "two");
        assert_eq!(got[1].role, "user");
        assert_eq!(got[1].content, "three");
    }

    #[test]
    fn skips_empty_content() {
        let dir = TempDir::new().expect("tempdir");
        let db = MemoryDb::open(dir.path().join("runtime.db")).expect("open");
        db.append_message("C1", "user", "   ").expect("empty");
        let got = db.get_last_messages("C1", 10).expect("get");
        assert!(got.is_empty());
    }
}

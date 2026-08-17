// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! `SQLite` ship audit log (`publish_audit` on the app state DB).

use crate::publish::{PublishError, PublishMode, PublishRequest, PublishResult};
use crate::sqlite::open_configured;
use chrono::Local;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::path::Path;
use thiserror::Error;

const SCHEMA_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/publish_audit_schema.sql"
));
const INSERT_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/publish_audit_insert.sql"
));
const GET_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/publish_audit_get.sql"
));

/// One row in `publish_audit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishAuditRow {
    pub id: i64,
    pub draft_id: Option<String>,
    pub pubs_pr_number: Option<u64>,
    pub mode: String,
    pub status: String,
    pub linkedin_urn: Option<String>,
    pub linkedin_url: Option<String>,
    pub error: Option<String>,
    pub body_preview: String,
    pub body_sha256: String,
    pub detail: String,
    pub created_at: String,
}

/// Input for a successful or failed ship attempt.
#[derive(Debug, Clone)]
pub struct PublishAuditWrite {
    pub draft_id: Option<String>,
    pub pubs_pr_number: Option<u64>,
    pub mode: PublishMode,
    pub status: &'static str,
    pub linkedin_urn: Option<String>,
    pub linkedin_url: Option<String>,
    pub error: Option<String>,
    pub body: String,
    pub detail: String,
}

impl PublishAuditWrite {
    /// Builds an `ok` row from a publish request + result.
    #[must_use]
    pub fn from_ok(request: &PublishRequest, result: &PublishResult) -> Self {
        Self {
            draft_id: request.draft_id.clone(),
            pubs_pr_number: request.pubs_pr_number,
            mode: result.mode,
            status: "ok",
            linkedin_urn: result.linkedin_urn.clone(),
            linkedin_url: result.linkedin_url.clone(),
            error: None,
            body: request.body.clone(),
            detail: result.detail.clone(),
        }
    }

    /// Builds an `error` row when publish fails after merge.
    #[must_use]
    pub fn from_err(request: &PublishRequest, mode: PublishMode, err: &PublishError) -> Self {
        Self {
            draft_id: request.draft_id.clone(),
            pubs_pr_number: request.pubs_pr_number,
            mode,
            status: "error",
            linkedin_urn: None,
            linkedin_url: None,
            error: Some(err.to_string()),
            body: request.body.clone(),
            detail: format!("publish failed: {err}"),
        }
    }
}

/// `SQLite` store for ship audit rows.
pub struct PublishAuditStore {
    conn: Connection,
}

#[derive(Debug, Error)]
pub enum PublishAuditError {
    #[error("open publish audit: {0}")]
    Open(#[from] rusqlite::Error),
    #[error("publish audit: {0}")]
    Other(String),
}

impl PublishAuditStore {
    /// Opens (or creates) the DB and ensures `publish_audit` exists.
    ///
    /// # Errors
    ///
    /// Returns [`PublishAuditError::Open`] on `SQLite` open failure, or [`PublishAuditError::Other`] for insert failure.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PublishAuditError> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    PublishAuditError::Other(format!("create parent {}: {e}", parent.display()))
                })?;
            }
        }
        let conn = open_configured(path.as_ref())?;
        let store = Self { conn };
        store
            .conn
            .execute_batch(SCHEMA_SQL)
            .map_err(PublishAuditError::Open)?;
        Ok(store)
    }

    /// Inserts one audit row; returns the new `id`.
    ///
    /// # Errors
    ///
    /// Returns [`PublishAuditError::Open`] on `SQLite` open failure, or [`PublishAuditError::Other`] for insert failure.
    pub fn insert(&self, row: &PublishAuditWrite) -> Result<i64, PublishAuditError> {
        let created_at = Local::now().to_rfc3339();
        let preview = body_preview(&row.body, 240);
        let sha = body_sha256(&row.body);
        let pubs = row.pubs_pr_number.map(u64::cast_signed);
        self.conn.execute(
            INSERT_SQL,
            params![
                row.draft_id,
                pubs,
                row.mode.as_str(),
                row.status,
                row.linkedin_urn,
                row.linkedin_url,
                row.error,
                preview,
                sha,
                row.detail,
                created_at,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Load one row by id (tests / debugging).
    ///
    /// # Errors
    ///
    /// Returns [`PublishAuditError::Open`] on `SQLite` open failure, or [`PublishAuditError::Other`] for insert failure.
    pub fn get(&self, id: i64) -> Result<Option<PublishAuditRow>, PublishAuditError> {
        let mut stmt = self.conn.prepare(GET_SQL)?;
        let row = stmt
            .query_row(params![id], |r| {
                Ok(PublishAuditRow {
                    id: r.get(0)?,
                    draft_id: r.get(1)?,
                    pubs_pr_number: r.get::<_, Option<i64>>(2)?.map(i64::cast_unsigned),
                    mode: r.get(3)?,
                    status: r.get(4)?,
                    linkedin_urn: r.get(5)?,
                    linkedin_url: r.get(6)?,
                    error: r.get(7)?,
                    body_preview: r.get(8)?,
                    body_sha256: r.get(9)?,
                    detail: r.get(10)?,
                    created_at: r.get(11)?,
                })
            })
            .optional()?;
        Ok(row)
    }
}

fn body_preview(body: &str, max_chars: usize) -> String {
    let trimmed = body.trim();
    let mut chars = trimmed.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}

fn body_sha256(body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::{PlaygroundPublisher, PublishRequest, Publisher};
    use tempfile::TempDir;

    #[tokio::test]
    async fn insert_ok_from_mock_publish() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("a.db");
        let store = PublishAuditStore::open(&path).expect("open");
        let request = PublishRequest {
            draft_id: Some("DRAFT-20260728-000022".into()),
            pubs_pr_number: Some(42),
            body: "Hello audit".into(),
        };
        let result = PlaygroundPublisher
            .publish_company_post(&request)
            .await
            .expect("mock");
        let id = store
            .insert(&PublishAuditWrite::from_ok(&request, &result))
            .expect("insert");
        let got = store.get(id).expect("get").expect("row");
        assert_eq!(got.status, "ok");
        assert_eq!(got.mode, "playground");
        assert_eq!(got.draft_id.as_deref(), Some("DRAFT-20260728-000022"));
        assert_eq!(got.pubs_pr_number, Some(42));
        assert!(got.linkedin_urn.is_some());
        assert_eq!(got.body_preview, "Hello audit");
        assert_eq!(got.body_sha256, body_sha256("Hello audit"));
        assert!(got.error.is_none());
    }

    #[test]
    fn insert_error_row() {
        let dir = TempDir::new().expect("temp");
        let store = PublishAuditStore::open(dir.path().join("e.db")).expect("open");
        let request = PublishRequest {
            draft_id: None,
            pubs_pr_number: Some(7),
            body: "x".into(),
        };
        let err = PublishError::Credentials("no token".into());
        let id = store
            .insert(&PublishAuditWrite::from_err(
                &request,
                PublishMode::Production,
                &err,
            ))
            .expect("insert");
        let got = store.get(id).expect("get").expect("row");
        assert_eq!(got.status, "error");
        assert_eq!(got.mode, "production");
        assert!(got.error.as_deref().unwrap_or("").contains("no token"));
    }
}

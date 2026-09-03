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
const LATEST_OK_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/publish_audit_latest_ok.sql"
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

    /// Latest successful ship for this artefact id and/or publications PR (BAT dedupe).
    ///
    /// When `draft_id` is set, only that artefact matches. Publications PR numbers are
    /// **not** unique across the `LinkedIn` vs X pubs repos (same `#79` can be an XPOST
    /// and a later `LinkedIn` POST).
    ///
    /// # Errors
    ///
    /// Returns [`PublishAuditError::Open`] on `SQLite` failure.
    pub fn latest_ok(
        &self,
        draft_id: Option<&str>,
        pubs_pr_number: Option<u64>,
    ) -> Result<Option<PublishAuditRow>, PublishAuditError> {
        let pubs = pubs_pr_number.map(u64::cast_signed);
        let mut stmt = self.conn.prepare(LATEST_OK_SQL)?;
        let row = stmt
            .query_row(params![draft_id, pubs], |r| {
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

    #[test]
    fn latest_ok_matches_draft_or_pubs_pr() {
        let dir = TempDir::new().expect("temp");
        let store = PublishAuditStore::open(dir.path().join("d.db")).expect("open");
        let request = PublishRequest {
            draft_id: Some("XPOST-20260902-000107".into()),
            pubs_pr_number: Some(116),
            body: "ship once".into(),
        };
        let result = PublishResult {
            mode: PublishMode::Production,
            linkedin_urn: Some("2095144454436864241".into()),
            linkedin_url: Some("https://x.com/interchouette/status/2095144454436864241".into()),
            detail: "https://x.com/interchouette/status/2095144454436864241".into(),
        };
        store
            .insert(&PublishAuditWrite::from_ok(&request, &result))
            .expect("insert");
        let by_id = store
            .latest_ok(Some("XPOST-20260902-000107"), None)
            .expect("query")
            .expect("row");
        assert_eq!(by_id.pubs_pr_number, Some(116));
        let by_pr = store
            .latest_ok(None, Some(116))
            .expect("query")
            .expect("row");
        assert_eq!(by_pr.draft_id.as_deref(), Some("XPOST-20260902-000107"));
        assert!(store
            .latest_ok(Some("OTHER"), Some(999))
            .expect("q")
            .is_none());
    }

    #[test]
    fn latest_ok_does_not_cross_match_linkedin_post_and_xpost_same_pr_number() {
        // POST-20260903-000148: LinkedIn fork PR #79 collided with XPOST-20260825-000074
        // (tweets repo PR #79). Dedupe must key on artefact id, not PR number alone.
        let dir = TempDir::new().expect("temp");
        let store = PublishAuditStore::open(dir.path().join("cross.db")).expect("open");
        let x_req = PublishRequest {
            draft_id: Some("XPOST-20260825-000074".into()),
            pubs_pr_number: Some(79),
            body: "old x ship".into(),
        };
        let x_result = PublishResult {
            mode: PublishMode::Production,
            linkedin_urn: Some("2092237166126551463".into()),
            linkedin_url: Some("https://x.com/interchouette/status/2092237166126551463".into()),
            detail: "https://x.com/interchouette/status/2092237166126551463\n\
Reply https://x.com/interchouette/status/2092237181477634212"
                .into(),
        };
        store
            .insert(&PublishAuditWrite::from_ok(&x_req, &x_result))
            .expect("insert x");
        let hit = store
            .latest_ok(Some("POST-20260903-000148"), Some(79))
            .expect("query");
        assert!(
            hit.is_none(),
            "LinkedIn POST must not reuse XPOST audit for same PR #: {hit:?}"
        );
        let same_x = store
            .latest_ok(Some("XPOST-20260825-000074"), Some(79))
            .expect("query")
            .expect("x row");
        assert!(same_x.detail.contains("x.com/interchouette/status"));
    }
}

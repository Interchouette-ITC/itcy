// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Persist operator drafts by Draft ID (lifecycle until Post).

use crate::sqlite::open_configured;
use chrono::Local;
use rusqlite::{params, Connection};
use std::path::Path;
use thiserror::Error;

const SCHEMA_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/drafts_schema.sql"
));
const UPSERT_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/drafts_upsert.sql"
));
const GET_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/drafts_get.sql"
));
const MARK_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/drafts_mark_status.sql"
));
const MARK_FROM_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/drafts_mark_from.sql"
));
const SET_FORK_PR_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/drafts_set_fork_pr.sql"
));
const FAIL_BUILDING_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/drafts_fail_building.sql"
));
const LIST_PREFIX_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/drafts_list_prefix.sql"
));
const USED_PROPOSE_SUBJECTS_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/drafts_used_propose_subjects.sql"
));
const DELETE_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/drafts_delete.sql"
));
const DELETE_PREFIX_STATUS_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/drafts_delete_prefix_status.sql"
));

/// Draft lifecycle statuses (`SQLite` `status` column).
pub mod status {
    pub const BUILDING: &str = "building";
    pub const OPEN: &str = "open";
    /// Fork Draft PR open; waiting gRoussac Approve (BAT).
    pub const ACCEPTED: &str = "accepted";
    /// Promoted to org Post + ship attempted.
    pub const PUBLISHED: &str = "published";
    pub const FAILED: &str = "failed";
}

/// One operator draft row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDraft {
    pub draft_id: String,
    pub subject: String,
    pub body: String,
    pub model: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub sources: Vec<String>,
    pub link_options: Vec<String>,
    pub research_pack: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub fork_pr_number: Option<u64>,
    pub fork_pr_url: String,
}

/// Legacy alias used by pack/BAT helpers.
pub type PendingDraft = StoredDraft;

/// `SQLite` store for drafts keyed by `draft_id`.
pub struct DraftStore {
    conn: Connection,
}

#[derive(Debug, Error)]
pub enum DraftStoreError {
    #[error("open draft store: {0}")]
    Open(#[from] rusqlite::Error),
    #[error("drafts: {0}")]
    Other(String),
}

impl DraftStore {
    /// Opens (or creates) the DB and ensures the drafts schema (+ migrates).
    ///
    /// # Errors
    ///
    /// Returns [`DraftStoreError::Open`] on `SQLite` open failure, or [`DraftStoreError::Other`] for query failures.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DraftStoreError> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    DraftStoreError::Other(format!("create parent {}: {e}", parent.display()))
                })?;
            }
        }
        let conn = open_configured(path.as_ref())?;
        let store = Self { conn };
        store.conn.execute_batch(SCHEMA_SQL)?;
        store.migrate_fork_pr_columns()?;
        store.migrate_legacy_pending_best_effort()?;
        Ok(store)
    }

    fn migrate_fork_pr_columns(&self) -> Result<(), DraftStoreError> {
        let cols: Vec<String> = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(drafts)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        if !cols.iter().any(|c| c == "fork_pr_number") {
            self.conn
                .execute("ALTER TABLE drafts ADD COLUMN fork_pr_number INTEGER", [])?;
        }
        if !cols.iter().any(|c| c == "fork_pr_url") {
            self.conn.execute(
                "ALTER TABLE drafts ADD COLUMN fork_pr_url TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        Ok(())
    }

    /// Insert or replace a draft (same `draft_id` for rework).
    ///
    /// # Errors
    ///
    /// Returns [`DraftStoreError`] on `SQLite` failure.
    pub fn upsert(&self, draft: &StoredDraft) -> Result<(), DraftStoreError> {
        let now = Local::now().to_rfc3339();
        let created = if draft.created_at.is_empty() {
            now.clone()
        } else {
            draft.created_at.clone()
        };
        let updated = if draft.updated_at.is_empty() {
            now
        } else {
            draft.updated_at.clone()
        };
        let status = if draft.status.is_empty() {
            status::OPEN
        } else {
            draft.status.as_str()
        };
        let sources_json = serde_json::to_string(&draft.sources)
            .map_err(|e| DraftStoreError::Other(e.to_string()))?;
        let links_json = serde_json::to_string(&draft.link_options)
            .map_err(|e| DraftStoreError::Other(e.to_string()))?;
        let pr_num = draft.fork_pr_number.map(|n| i64::try_from(n).unwrap_or(0));
        self.conn.execute(
            UPSERT_SQL,
            params![
                draft.draft_id,
                draft.subject,
                draft.body,
                draft.model,
                draft.tokens_in,
                draft.tokens_out,
                sources_json,
                links_json,
                draft.research_pack,
                status,
                created,
                updated,
                pr_num,
                draft.fork_pr_url,
            ],
        )?;
        Ok(())
    }

    /// Load one draft by id.
    ///
    /// # Errors
    ///
    /// Returns [`DraftStoreError`] on `SQLite` failure.
    pub fn get(&self, draft_id: &str) -> Result<Option<StoredDraft>, DraftStoreError> {
        let mut stmt = self.conn.prepare(GET_SQL)?;
        let mut rows = stmt.query(params![draft_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(row_to_draft(row)?))
    }

    /// Mark an **open** draft to `status` (e.g. accepted). Returns false if not open / missing.
    ///
    /// # Errors
    ///
    /// Returns [`DraftStoreError`] on `SQLite` failure.
    pub fn mark_status(&self, draft_id: &str, status: &str) -> Result<bool, DraftStoreError> {
        let updated = Local::now().to_rfc3339();
        let n = self
            .conn
            .execute(MARK_SQL, params![draft_id, status, updated])?;
        Ok(n > 0)
    }

    /// Transition `from` → `to` (e.g. accepted → published, accepted → open).
    ///
    /// # Errors
    ///
    /// Returns [`DraftStoreError`] on `SQLite` failure.
    pub fn mark_status_from(
        &self,
        draft_id: &str,
        from: &str,
        to: &str,
    ) -> Result<bool, DraftStoreError> {
        let updated = Local::now().to_rfc3339();
        let n = self
            .conn
            .execute(MARK_FROM_SQL, params![draft_id, to, updated, from])?;
        Ok(n > 0)
    }

    /// Persist fork Draft PR coordinates (does not change status).
    ///
    /// # Errors
    ///
    /// Returns [`DraftStoreError`] on `SQLite` failure.
    pub fn set_fork_pr(
        &self,
        draft_id: &str,
        pr_number: u64,
        pr_url: &str,
    ) -> Result<(), DraftStoreError> {
        let updated = Local::now().to_rfc3339();
        let n = i64::try_from(pr_number).unwrap_or(0);
        self.conn
            .execute(SET_FORK_PR_SQL, params![draft_id, n, pr_url, updated])?;
        Ok(())
    }

    /// Mark every in-flight `building` draft as `failed` (product restart). Returns rows touched.
    ///
    /// # Errors
    ///
    /// Returns [`DraftStoreError`] on `SQLite` failure.
    pub fn fail_all_building(&self) -> Result<usize, DraftStoreError> {
        let updated = Local::now().to_rfc3339();
        let n = self.conn.execute(FAIL_BUILDING_SQL, params![updated])?;
        Ok(n)
    }

    /// Newest rows whose id starts with `prefix` (e.g. `TWEET-`), capped at 50.
    ///
    /// # Errors
    ///
    /// Returns [`DraftStoreError`] on `SQLite` failure.
    pub fn list_by_id_prefix(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<StoredDraft>, DraftStoreError> {
        let cap = i64::try_from(limit.clamp(1, 50)).unwrap_or(30);
        let like = format!("{prefix}%");
        let mut stmt = self.conn.prepare(LIST_PREFIX_SQL)?;
        let mut rows = stmt.query(params![like, cap])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_draft(row)?);
        }
        Ok(out)
    }

    /// Subjects already used by in-flight or shipped `DRAFT-` / `TWEET-` rows (newest first).
    ///
    /// Used by bare `/propose_*` to skip corpus angles that already have a draft or tweet.
    ///
    /// # Errors
    ///
    /// Returns [`DraftStoreError`] on `SQLite` failure.
    pub fn used_propose_subjects(&self, limit: usize) -> Result<Vec<String>, DraftStoreError> {
        let cap = i64::try_from(limit.clamp(1, 500)).unwrap_or(200);
        let mut stmt = self.conn.prepare(USED_PROPOSE_SUBJECTS_SQL)?;
        let mut rows = stmt.query(params![cap])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let subject: String = row.get(0)?;
            let t = subject.trim();
            if t.is_empty() {
                continue;
            }
            out.push(t.to_string());
        }
        Ok(out)
    }

    /// Delete one row by id. Returns true when a row was removed.
    ///
    /// # Errors
    ///
    /// Returns [`DraftStoreError`] on `SQLite` failure.
    pub fn delete(&self, draft_id: &str) -> Result<bool, DraftStoreError> {
        let n = self.conn.execute(DELETE_SQL, params![draft_id])?;
        Ok(n > 0)
    }

    /// Delete every row with `prefix` and `status` (e.g. drop published tweets).
    ///
    /// # Errors
    ///
    /// Returns [`DraftStoreError`] on `SQLite` failure.
    pub fn delete_prefix_status(
        &self,
        prefix: &str,
        status: &str,
    ) -> Result<usize, DraftStoreError> {
        let like = format!("{prefix}%");
        let n = self
            .conn
            .execute(DELETE_PREFIX_STATUS_SQL, params![like, status])?;
        Ok(n)
    }

    /// Operator-facing gate reason when a command is not allowed for this status.
    #[must_use]
    pub fn gate_message(draft_id: &str, status: &str, want: &str) -> String {
        match status {
            status::BUILDING => format!(
                "Draft `{draft_id}` is still **building** (LOAD/writer). Wait for Slack, or `/draft_about` again if it stalled."
            ),
            status::FAILED => format!(
                "Draft `{draft_id}` **failed** mid-build. Run `/draft_about` for a new draft."
            ),
            status::ACCEPTED => format!(
                "Draft `{draft_id}` has a fork PR (**accepted** / waiting BAT). \
`/accept_draft {draft_id}` again re-syncs the same PR (and publishes the Post if Approve is already on GitHub but the webhook missed). \
`/retry_bat {draft_id}` re-ships after BAT (missed webhook or ship failed). Rework / change_url still work for content edits. Cannot `{want}`."
            ),
            status::PUBLISHED => format!(
                "Draft `{draft_id}` is already a **Post** (`published`). Do not rework or accept again."
            ),
            other => format!("Draft `{draft_id}` status=`{other}`; cannot `{want}`."),
        }
    }

    fn migrate_legacy_pending_best_effort(&self) -> Result<(), DraftStoreError> {
        let has_legacy: bool = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pending_drafts'",
                [],
                |r| r.get::<_, i64>(0).map(|n| n > 0),
            )
            .unwrap_or(false);
        if !has_legacy {
            return Ok(());
        }
        let row: Result<(String, String, String, i64, i64, String, String), _> =
            self.conn.query_row(
                "SELECT subject, body, model, tokens_in, tokens_out, sources_json, created_at \
             FROM pending_drafts WHERE id = 1",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            );
        let Ok((subject, body, model, tin, tout, sources_json, created_at)) = row else {
            return Ok(());
        };
        let draft_id = extract_draft_id_from_body(&body)
            .unwrap_or_else(|| format!("DRAFT-{}-LEGACY", Local::now().format("%Y%m%d")));
        if self.get(&draft_id)?.is_some() {
            return Ok(());
        }
        let sources: Vec<String> = serde_json::from_str(&sources_json).unwrap_or_default();
        let draft = StoredDraft {
            draft_id,
            subject,
            body,
            model,
            tokens_in: u32::try_from(tin).unwrap_or(0),
            tokens_out: u32::try_from(tout).unwrap_or(0),
            sources,
            link_options: Vec::new(),
            research_pack: String::new(),
            status: status::OPEN.into(),
            created_at: created_at.clone(),
            updated_at: created_at,
            fork_pr_number: None,
            fork_pr_url: String::new(),
        };
        self.upsert(&draft)?;
        Ok(())
    }
}

fn row_to_draft(row: &rusqlite::Row<'_>) -> Result<StoredDraft, DraftStoreError> {
    let sources_json: String = row.get(6)?;
    let links_json: String = row.get(7)?;
    let sources: Vec<String> = serde_json::from_str(&sources_json).unwrap_or_default();
    let link_options: Vec<String> = serde_json::from_str(&links_json).unwrap_or_default();
    let fork_pr_number: Option<u64> = row
        .get::<_, Option<i64>>(12)
        .ok()
        .flatten()
        .and_then(|n| u64::try_from(n).ok());
    let fork_pr_url: String = row.get::<_, String>(13).unwrap_or_default();
    Ok(StoredDraft {
        draft_id: row.get(0)?,
        subject: row.get(1)?,
        body: row.get(2)?,
        model: row.get(3)?,
        tokens_in: u32::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
        tokens_out: u32::try_from(row.get::<_, i64>(5)?).unwrap_or(0),
        sources,
        link_options,
        research_pack: row.get(8)?,
        status: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        fork_pr_number,
        fork_pr_url,
    })
}

fn extract_draft_id_from_body(body: &str) -> Option<String> {
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Draft ID:") {
            let id = rest.trim();
            if id.starts_with("DRAFT-") {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Build a new open draft row from grounded-draft parts.
#[derive(Debug, Clone)]
pub struct DraftPayload {
    pub draft_id: String,
    pub subject: String,
    pub body: String,
    pub model: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub sources: Vec<String>,
    pub link_options: Vec<String>,
    pub research_pack: String,
}

/// Build a new open draft row from a payload.
#[must_use]
pub fn stored_from_payload(p: DraftPayload) -> StoredDraft {
    let now = Local::now().to_rfc3339();
    StoredDraft {
        draft_id: p.draft_id,
        subject: p.subject,
        body: p.body,
        model: p.model,
        tokens_in: p.tokens_in,
        tokens_out: p.tokens_out,
        sources: p.sources,
        link_options: p.link_options,
        research_pack: p.research_pack,
        status: status::OPEN.into(),
        created_at: now.clone(),
        updated_at: now,
        fork_pr_number: None,
        fork_pr_url: String::new(),
    }
}

/// Stub row at `/draft_about` start (`building`).
#[must_use]
pub fn stored_building_stub(draft_id: &str, subject: &str) -> StoredDraft {
    let now = Local::now().to_rfc3339();
    StoredDraft {
        draft_id: draft_id.to_string(),
        subject: subject.to_string(),
        body: String::new(),
        model: String::new(),
        tokens_in: 0,
        tokens_out: 0,
        sources: Vec::new(),
        link_options: Vec::new(),
        research_pack: String::new(),
        status: status::BUILDING.into(),
        created_at: now.clone(),
        updated_at: now,
        fork_pr_number: None,
        fork_pr_url: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn upsert_get_and_accept_roundtrip() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("d.db");
        let store = DraftStore::open(&path).expect("open");
        let draft = stored_from_payload(DraftPayload {
            draft_id: "DRAFT-20260728-000001".into(),
            subject: "rust".into(),
            body: "Draft ID: DRAFT-20260728-000001\n\nbody text".into(),
            model: "mock/m".into(),
            tokens_in: 1,
            tokens_out: 2,
            sources: vec!["https://example.com".into()],
            link_options: vec!["https://example.com".into()],
            research_pack: "pack".into(),
        });
        store.upsert(&draft).expect("upsert");
        let got = store
            .get("DRAFT-20260728-000001")
            .expect("get")
            .expect("some");
        assert_eq!(got.subject, "rust");
        assert_eq!(got.status, status::OPEN);
        assert_eq!(got.research_pack, "pack");
        assert!(store
            .mark_status("DRAFT-20260728-000001", status::ACCEPTED)
            .expect("mark"));
        let accepted = store
            .get("DRAFT-20260728-000001")
            .expect("get2")
            .expect("some");
        assert_eq!(accepted.status, status::ACCEPTED);
        assert!(!store
            .mark_status("DRAFT-20260728-000001", status::ACCEPTED)
            .expect("already"));
    }

    #[test]
    fn building_checkpoint_reopen_and_fail_stale() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("d.db");
        let store = DraftStore::open(&path).expect("open");
        store
            .upsert(&stored_building_stub("DRAFT-20260803-000099", "topic"))
            .expect("stub");
        let mut row = store.get("DRAFT-20260803-000099").expect("g").expect("s");
        assert_eq!(row.status, status::BUILDING);
        row.research_pack = "pack text".into();
        row.updated_at = String::new();
        store.upsert(&row).expect("pack");
        assert_eq!(
            store
                .get("DRAFT-20260803-000099")
                .expect("g2")
                .expect("s")
                .research_pack,
            "pack text"
        );
        assert_eq!(store.fail_all_building().expect("fail"), 1);
        assert_eq!(
            store
                .get("DRAFT-20260803-000099")
                .expect("g3")
                .expect("s")
                .status,
            status::FAILED
        );

        let open = stored_from_payload(DraftPayload {
            draft_id: "DRAFT-20260803-000100".into(),
            subject: "x".into(),
            body: "body".into(),
            model: "m".into(),
            tokens_in: 0,
            tokens_out: 0,
            sources: vec![],
            link_options: vec![],
            research_pack: String::new(),
        });
        store.upsert(&open).expect("open");
        store
            .mark_status("DRAFT-20260803-000100", status::ACCEPTED)
            .expect("acc");
        store
            .set_fork_pr(
                "DRAFT-20260803-000100",
                1,
                "https://github.com/Interchouette/itcy-publications/pull/1",
            )
            .expect("pr");
        assert!(store
            .mark_status_from("DRAFT-20260803-000100", status::ACCEPTED, status::OPEN)
            .expect("reopen"));
        let reopened = store.get("DRAFT-20260803-000100").expect("g4").expect("s");
        assert_eq!(reopened.status, status::OPEN);
        assert_eq!(reopened.fork_pr_number, Some(1));
    }

    #[test]
    fn list_tweets_and_delete_row() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("d.db");
        let store = DraftStore::open(&path).expect("open");
        store
            .upsert(&stored_from_payload(DraftPayload {
                draft_id: "DRAFT-20260814-000001".into(),
                subject: "linkedin".into(),
                body: "li".into(),
                model: "m".into(),
                tokens_in: 0,
                tokens_out: 0,
                sources: vec![],
                link_options: vec![],
                research_pack: String::new(),
            }))
            .expect("li");
        store
            .upsert(&stored_from_payload(DraftPayload {
                draft_id: "TWEET-20260814-000001".into(),
                subject: "owl".into(),
                body: "hi".into(),
                model: "m".into(),
                tokens_in: 0,
                tokens_out: 0,
                sources: vec![],
                link_options: vec![],
                research_pack: String::new(),
            }))
            .expect("tw");
        let tweets = store.list_by_id_prefix("TWEET-", 10).expect("list");
        assert_eq!(tweets.len(), 1);
        assert_eq!(tweets[0].draft_id, "TWEET-20260814-000001");
        assert!(store.delete("TWEET-20260814-000001").expect("del"));
        assert!(store
            .list_by_id_prefix("TWEET-", 10)
            .expect("empty")
            .is_empty());
        assert!(store.get("DRAFT-20260814-000001").expect("keep").is_some());
        assert!(!store.delete("TWEET-20260814-000001").expect("gone"));

        store
            .upsert(&stored_from_payload(DraftPayload {
                draft_id: "TWEET-20260814-000002".into(),
                subject: "shipped".into(),
                body: "hi".into(),
                model: "m".into(),
                tokens_in: 0,
                tokens_out: 0,
                sources: vec![],
                link_options: vec![],
                research_pack: String::new(),
            }))
            .expect("pub");
        assert!(store
            .mark_status("TWEET-20260814-000002", status::ACCEPTED)
            .expect("acc"));
        assert!(store
            .mark_status_from("TWEET-20260814-000002", status::ACCEPTED, status::PUBLISHED)
            .expect("pubst"));
        assert!(store
            .list_by_id_prefix("TWEET-", 10)
            .expect("hide")
            .is_empty());
        assert_eq!(
            store
                .delete_prefix_status("TWEET-", status::PUBLISHED)
                .expect("purge"),
            1
        );
        assert!(store.get("TWEET-20260814-000002").expect("gone").is_none());
    }
}

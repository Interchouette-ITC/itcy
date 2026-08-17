// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! `SQLite` sources + embedding chunks for subject RAG and `LinkedIn` activity queries.

use crate::sqlite::open_configured;
use chrono::Local;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use thiserror::Error;

const SOURCES_SCHEMA_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/sources_schema.sql"
));
const SOURCES_INSERT_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/sources_insert.sql"
));
const SOURCES_COUNT_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/sources_count.sql"
));
const SOURCES_EXISTS_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/sources_exists.sql"
));
const SOURCES_COUNTS_BY_ACTIVITY_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/sources_counts_by_activity.sql"
));
const SOURCES_GET_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/sources_get.sql"
));
const SOURCES_LIST_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/sources_list.sql"
));
const SOURCES_SEARCH_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/sources_search.sql"
));
const CHUNKS_SCHEMA_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/chunks_schema.sql"
));
const CHUNKS_INSERT_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/chunks_insert.sql"
));
const CHUNKS_GET_CANDIDATES_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/chunks_get_candidates.sql"
));
const CHUNKS_LIST_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/chunks_list.sql"
));
const CHUNKS_COUNT_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/chunks_count.sql"
));

/// One ingested source document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub id: i64,
    pub kind: String,
    pub activity: String,
    pub subject: String,
    pub title: String,
    pub url: Option<String>,
    pub raw_text: String,
    pub occurred_at: Option<String>,
    pub enrich_status: String,
    pub enrich_after: Option<String>,
    pub enrich_claimed_at: Option<String>,
    pub enriched_at: Option<String>,
}

/// Enrich queue counters for MCP / logs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnrichStatusCounts {
    pub pending: u64,
    pub in_flight: u64,
    pub ok: u64,
    pub failed: u64,
    pub skip: u64,
    pub none: u64,
    pub next_enrich_after: Option<String>,
}

/// List/search row with text preview (not full body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceListItem {
    pub id: i64,
    pub kind: String,
    pub activity: String,
    pub subject: String,
    pub title: String,
    pub url: Option<String>,
    pub text_len: i64,
    pub preview: String,
    pub occurred_at: Option<String>,
}

/// Chunk with decoded embedding vector.
#[derive(Debug, Clone)]
pub struct ChunkRecord {
    pub id: i64,
    pub source_id: i64,
    pub subject: String,
    pub text: String,
    pub embedding: Vec<f32>,
}

/// Chunk list row without embedding blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkListItem {
    pub id: i64,
    pub source_id: i64,
    pub subject: String,
    pub preview: String,
    pub text_len: i64,
}

/// Count row for stats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityCount {
    pub kind: String,
    pub activity: String,
    pub count: u64,
}

/// Filter for listing sources.
#[derive(Debug, Clone, Default)]
pub struct SourceListFilter {
    pub activity: String,
    pub subject_contains: String,
    pub title_contains: String,
    pub preview_chars: u32,
    pub limit: u32,
    pub offset: u32,
}

/// Arguments for inserting one source row.
#[derive(Debug, Clone)]
pub struct InsertSource<'a> {
    pub kind: &'a str,
    pub activity: &'a str,
    pub subject: &'a str,
    pub title: &'a str,
    pub url: Option<&'a str>,
    pub raw_text: &'a str,
    pub occurred_at: Option<&'a str>,
}

/// SQLite-backed source + chunk store (same DB file as message memory is fine).
pub struct SourceDb {
    conn: Connection,
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("open source db: {0}")]
    Open(#[from] rusqlite::Error),
    #[error("sources: {0}")]
    Other(String),
}

impl SourceDb {
    /// Opens (or creates) the DB and ensures sources/chunks schemas exist.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SourceError> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    SourceError::Other(format!("create parent {}: {e}", parent.display()))
                })?;
            }
        }
        let conn = open_configured(path.as_ref())?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    /// Runs `f` inside a single write transaction (bulk import).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn with_transaction<T, F>(&self, f: F) -> Result<T, SourceError>
    where
        F: FnOnce(&Connection) -> Result<T, SourceError>,
    {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| SourceError::Other(format!("begin transaction: {e}")))?;
        let out = f(&tx)?;
        tx.commit()
            .map_err(|e| SourceError::Other(format!("commit transaction: {e}")))?;
        Ok(out)
    }

    fn init(&self) -> Result<(), SourceError> {
        self.conn
            .execute_batch(SOURCES_SCHEMA_SQL)
            .map_err(|e| SourceError::Other(format!("sources schema: {e}")))?;
        self.migrate_sources_columns()?;
        self.conn
            .execute_batch(CHUNKS_SCHEMA_SQL)
            .map_err(|e| SourceError::Other(format!("chunks schema: {e}")))?;
        self.purge_reactions()?;
        Ok(())
    }

    /// Adds `activity/occurred_at/enrich`_* on older DBs.
    fn migrate_sources_columns(&self) -> Result<(), SourceError> {
        let cols = self.table_columns("sources")?;
        if !cols.iter().any(|c| c == "activity") {
            self.conn
                .execute(
                    "ALTER TABLE sources ADD COLUMN activity TEXT NOT NULL DEFAULT 'unknown'",
                    [],
                )
                .map_err(|e| SourceError::Other(format!("add activity: {e}")))?;
        }
        if !cols.iter().any(|c| c == "occurred_at") {
            self.conn
                .execute("ALTER TABLE sources ADD COLUMN occurred_at TEXT", [])
                .map_err(|e| SourceError::Other(format!("add occurred_at: {e}")))?;
        }
        let cols = self.table_columns("sources")?;
        for (name, ddl) in [
            (
                "enrich_status",
                "ALTER TABLE sources ADD COLUMN enrich_status TEXT NOT NULL DEFAULT 'none'",
            ),
            (
                "enrich_after",
                "ALTER TABLE sources ADD COLUMN enrich_after TEXT",
            ),
            (
                "enrich_claimed_at",
                "ALTER TABLE sources ADD COLUMN enrich_claimed_at TEXT",
            ),
            (
                "enriched_at",
                "ALTER TABLE sources ADD COLUMN enriched_at TEXT",
            ),
        ] {
            if !cols.iter().any(|c| c == name) {
                self.conn
                    .execute(ddl, [])
                    .map_err(|e| SourceError::Other(format!("add {name}: {e}")))?;
            }
        }
        self.conn
            .execute_batch(
                "CREATE INDEX IF NOT EXISTS sources_activity_occurred ON sources (activity, occurred_at DESC);
                 CREATE UNIQUE INDEX IF NOT EXISTS sources_dedupe
                 ON sources (activity, IFNULL(url, ''), IFNULL(occurred_at, ''), title);
                 CREATE INDEX IF NOT EXISTS sources_enrich_queue
                 ON sources (enrich_status, enrich_after);",
            )
            .map_err(|e| SourceError::Other(format!("sources indexes: {e}")))?;
        Ok(())
    }

    /// Deletes reaction / like rows and their chunks (out of inspiration scope).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn purge_reactions(&self) -> Result<u64, SourceError> {
        self.conn
            .execute_batch(
                "DELETE FROM chunks WHERE source_id IN (SELECT id FROM sources WHERE activity = 'reaction');
                 DELETE FROM sources WHERE activity = 'reaction';",
            )
            .map_err(|e| SourceError::Other(format!("purge reactions: {e}")))?;
        Ok(self.conn.changes())
    }

    fn table_columns(&self, table: &str) -> Result<Vec<String>, SourceError> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|e| SourceError::Other(format!("pragma table_info: {e}")))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| SourceError::Other(format!("pragma map: {e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| SourceError::Other(format!("pragma row: {e}")))
    }

    /// True when a dedupe key already exists (skip embed on weekly re-import).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn source_exists(
        &self,
        activity: &str,
        url: Option<&str>,
        occurred_at: Option<&str>,
        title: &str,
    ) -> Result<bool, SourceError> {
        Self::source_exists_on(&self.conn, activity, url, occurred_at, title)
    }

    /// Exists check on an open connection or transaction.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn source_exists_on(
        conn: &Connection,
        activity: &str,
        url: Option<&str>,
        occurred_at: Option<&str>,
        title: &str,
    ) -> Result<bool, SourceError> {
        let found: Option<i64> = conn
            .query_row(
                SOURCES_EXISTS_SQL,
                params![activity, url, occurred_at, title],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| SourceError::Other(format!("exists: {e}")))?;
        Ok(found.is_some())
    }

    /// Inserts a source row; returns its id, or `None` when the dedupe key already exists.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn insert_source(&self, row: &InsertSource<'_>) -> Result<Option<i64>, SourceError> {
        Self::insert_source_on(&self.conn, row)
    }

    /// Inserts a source row on an open connection or transaction.
    ///
    /// Returns `Ok(None)` on unique conflict (weekly re-import safe).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn insert_source_on(
        conn: &Connection,
        row: &InsertSource<'_>,
    ) -> Result<Option<i64>, SourceError> {
        let now = Local::now().to_rfc3339();
        conn.execute(
            SOURCES_INSERT_SQL,
            params![
                row.kind,
                row.activity,
                row.subject,
                row.title,
                row.url,
                row.raw_text,
                row.occurred_at,
                now
            ],
        )
        .map_err(|e| SourceError::Other(format!("insert source: {e}")))?;
        if conn.changes() == 0 {
            return Ok(None);
        }
        Ok(Some(conn.last_insert_rowid()))
    }

    /// Inserts a chunk with embedding (stored as little-endian f32 BLOB).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn insert_chunk(
        &self,
        source_id: i64,
        subject: &str,
        text: &str,
        embedding: &[f32],
    ) -> Result<i64, SourceError> {
        Self::insert_chunk_on(&self.conn, source_id, subject, text, embedding)
    }

    /// Inserts a chunk on an open connection or transaction.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn insert_chunk_on(
        conn: &Connection,
        source_id: i64,
        subject: &str,
        text: &str,
        embedding: &[f32],
    ) -> Result<i64, SourceError> {
        let now = Local::now().to_rfc3339();
        let blob: Vec<u8> = embedding.iter().flat_map(|&f| f.to_le_bytes()).collect();
        conn.execute(
            CHUNKS_INSERT_SQL,
            params![source_id, subject, text, blob, now],
        )
        .map_err(|e| SourceError::Other(format!("insert chunk: {e}")))?;
        Ok(conn.last_insert_rowid())
    }

    /// Candidate chunks for subject filter (empty subject = all), newest first.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    ///
    /// # Panics
    ///
    /// Panics when a lock is poisoned or an invariant is violated.
    pub fn get_chunk_candidates(
        &self,
        subject_filter: &str,
        limit: u32,
    ) -> Result<Vec<ChunkRecord>, SourceError> {
        let mut stmt = self
            .conn
            .prepare(CHUNKS_GET_CANDIDATES_SQL)
            .map_err(|e| SourceError::Other(format!("prepare candidates: {e}")))?;
        let rows = stmt
            .query_map(params![subject_filter, i64::from(limit)], |row| {
                let blob: Vec<u8> = row.get(4)?;
                if !blob.len().is_multiple_of(4) {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                let embedding: Vec<f32> = blob
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                    .collect();
                Ok(ChunkRecord {
                    id: row.get(0)?,
                    source_id: row.get(1)?,
                    subject: row.get(2)?,
                    text: row.get(3)?,
                    embedding,
                })
            })
            .map_err(|e| SourceError::Other(format!("query candidates: {e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| SourceError::Other(format!("row candidates: {e}")))
    }

    /// Total number of sources.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn source_count(&self) -> Result<u64, SourceError> {
        let count: i64 = self
            .conn
            .query_row(SOURCES_COUNT_SQL, [], |row| row.get(0))
            .map_err(|e| SourceError::Other(format!("count: {e}")))?;
        Ok(count.cast_unsigned())
    }

    /// Total number of chunks.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn chunk_count(&self) -> Result<u64, SourceError> {
        let count: i64 = self
            .conn
            .query_row(CHUNKS_COUNT_SQL, [], |row| row.get(0))
            .map_err(|e| SourceError::Other(format!("chunk count: {e}")))?;
        Ok(count.cast_unsigned())
    }

    /// Counts grouped by kind + activity.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn counts_by_activity(&self) -> Result<Vec<ActivityCount>, SourceError> {
        let mut stmt = self
            .conn
            .prepare(SOURCES_COUNTS_BY_ACTIVITY_SQL)
            .map_err(|e| SourceError::Other(format!("prepare counts: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ActivityCount {
                    kind: row.get(0)?,
                    activity: row.get(1)?,
                    count: row.get::<_, i64>(2)?.cast_unsigned(),
                })
            })
            .map_err(|e| SourceError::Other(format!("query counts: {e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| SourceError::Other(format!("row counts: {e}")))
    }

    /// Full source by id.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn get_source(&self, id: i64) -> Result<Option<SourceRecord>, SourceError> {
        let row = self
            .conn
            .query_row(SOURCES_GET_SQL, params![id], |row| {
                Ok(SourceRecord {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    activity: row.get(2)?,
                    subject: row.get(3)?,
                    title: row.get(4)?,
                    url: row.get(5)?,
                    raw_text: row.get(6)?,
                    occurred_at: row.get(7)?,
                    enrich_status: row
                        .get::<_, Option<String>>(8)?
                        .unwrap_or_else(|| "none".into()),
                    enrich_after: row.get(9)?,
                    enrich_claimed_at: row.get(10)?,
                    enriched_at: row.get(11)?,
                })
            })
            .optional()
            .map_err(|e| SourceError::Other(format!("get source: {e}")))?;
        Ok(row)
    }

    /// Lists sources with filters, newest `LinkedIn` time first.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn list_sources(
        &self,
        filter: &SourceListFilter,
    ) -> Result<Vec<SourceListItem>, SourceError> {
        let preview = i64::from(filter.preview_chars.max(40));
        let limit = i64::from(filter.limit.max(1));
        let offset = i64::from(filter.offset);
        let mut stmt = self
            .conn
            .prepare(SOURCES_LIST_SQL)
            .map_err(|e| SourceError::Other(format!("prepare list: {e}")))?;
        let rows = stmt
            .query_map(
                params![
                    filter.activity,
                    filter.subject_contains,
                    filter.title_contains,
                    preview,
                    limit,
                    offset
                ],
                |row| {
                    Ok(SourceListItem {
                        id: row.get(0)?,
                        kind: row.get(1)?,
                        activity: row.get(2)?,
                        subject: row.get(3)?,
                        title: row.get(4)?,
                        url: row.get(5)?,
                        text_len: row.get(6)?,
                        preview: row.get(7)?,
                        occurred_at: row.get(8)?,
                    })
                },
            )
            .map_err(|e| SourceError::Other(format!("query list: {e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| SourceError::Other(format!("row list: {e}")))
    }

    /// Case-insensitive search in title + `raw_text`.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn search_sources(
        &self,
        query: &str,
        activity: &str,
        preview_chars: u32,
        limit: u32,
    ) -> Result<Vec<SourceListItem>, SourceError> {
        let preview = i64::from(preview_chars.max(40));
        let limit = i64::from(limit.max(1));
        let mut stmt = self
            .conn
            .prepare(SOURCES_SEARCH_SQL)
            .map_err(|e| SourceError::Other(format!("prepare search: {e}")))?;
        let rows = stmt
            .query_map(params![activity, query, preview, limit], |row| {
                Ok(SourceListItem {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    activity: row.get(2)?,
                    subject: row.get(3)?,
                    title: row.get(4)?,
                    url: row.get(5)?,
                    text_len: row.get(6)?,
                    preview: row.get(7)?,
                    occurred_at: row.get(8)?,
                })
            })
            .map_err(|e| SourceError::Other(format!("query search: {e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| SourceError::Other(format!("row search: {e}")))
    }

    /// Lists chunks (optional source / subject filter).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn list_chunks(
        &self,
        source_id: Option<i64>,
        subject_contains: &str,
        preview_chars: u32,
        limit: u32,
    ) -> Result<Vec<ChunkListItem>, SourceError> {
        let sid = source_id.unwrap_or(0);
        let preview = i64::from(preview_chars.max(40));
        let limit = i64::from(limit.max(1));
        let mut stmt = self
            .conn
            .prepare(CHUNKS_LIST_SQL)
            .map_err(|e| SourceError::Other(format!("prepare chunks list: {e}")))?;
        let rows = stmt
            .query_map(params![sid, subject_contains, preview, limit], |row| {
                Ok(ChunkListItem {
                    id: row.get(0)?,
                    source_id: row.get(1)?,
                    subject: row.get(2)?,
                    preview: row.get(3)?,
                    text_len: row.get(4)?,
                })
            })
            .map_err(|e| SourceError::Other(format!("query chunks list: {e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| SourceError::Other(format!("row chunks list: {e}")))
    }

    /// Deletes all chunks for one source (before in-place re-embed).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn delete_chunks_for_source(&self, source_id: i64) -> Result<(), SourceError> {
        self.conn
            .execute(
                "DELETE FROM chunks WHERE source_id = ?1",
                params![source_id],
            )
            .map_err(|e| SourceError::Other(format!("delete chunks: {e}")))?;
        Ok(())
    }

    /// Marks link-only post/repost stubs as `pending` when still `none`.
    ///
    /// Never touches `ok` / `skip` / `in_flight` / `failed`.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn ensure_enrich_stub_queue(&self) -> Result<u64, SourceError> {
        let n = self
            .conn
            .execute(
                "UPDATE sources
                 SET enrich_status = 'pending'
                 WHERE activity IN ('post', 'repost')
                   AND url IS NOT NULL
                   AND TRIM(url) != ''
                   AND enrich_status = 'none'
                   AND (
                     raw_text LIKE 'Post%'
                     OR raw_text LIKE 'Repost%'
                     OR length(raw_text) < 120
                   )",
                [],
            )
            .map_err(|e| SourceError::Other(format!("ensure enrich queue: {e}")))?;
        Ok(n as u64)
    }

    /// Resets stale `in_flight` rows to `pending` (laptop died mid-fetch).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn reclaim_stale_enrich_claims(
        &self,
        older_than_rfc3339: &str,
    ) -> Result<u64, SourceError> {
        let n = self
            .conn
            .execute(
                "UPDATE sources
                 SET enrich_status = 'pending', enrich_claimed_at = NULL
                 WHERE enrich_status = 'in_flight'
                   AND (enrich_claimed_at IS NULL OR enrich_claimed_at < ?1)",
                params![older_than_rfc3339],
            )
            .map_err(|e| SourceError::Other(format!("reclaim enrich: {e}")))?;
        Ok(n as u64)
    }

    /// Claims the next due enrich row (newest first). Returns None when queue empty.
    ///
    /// Never selects `ok` / `skip` / fresh `in_flight`.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn claim_next_enrich(
        &self,
        now_rfc3339: &str,
    ) -> Result<Option<SourceRecord>, SourceError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| SourceError::Other(format!("begin claim: {e}")))?;
        let id: Option<i64> = tx
            .query_row(
                "SELECT id FROM sources
                 WHERE activity IN ('post', 'repost')
                   AND url IS NOT NULL AND TRIM(url) != ''
                   AND enrich_status IN ('pending', 'failed', 'none')
                   AND (enrich_after IS NULL OR enrich_after <= ?1)
                   AND (
                     enrich_status IN ('pending', 'failed')
                     OR raw_text LIKE 'Post%'
                     OR raw_text LIKE 'Repost%'
                     OR length(raw_text) < 120
                   )
                 ORDER BY occurred_at DESC, id DESC
                 LIMIT 1",
                params![now_rfc3339],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| SourceError::Other(format!("pick enrich: {e}")))?;
        let Some(id) = id else {
            tx.commit()
                .map_err(|e| SourceError::Other(format!("commit empty claim: {e}")))?;
            return Ok(None);
        };
        let updated = tx
            .execute(
                "UPDATE sources
                 SET enrich_status = 'in_flight', enrich_claimed_at = ?1
                 WHERE id = ?2 AND enrich_status NOT IN ('ok', 'skip')",
                params![now_rfc3339, id],
            )
            .map_err(|e| SourceError::Other(format!("claim update: {e}")))?;
        if updated == 0 {
            tx.commit()
                .map_err(|e| SourceError::Other(format!("commit race claim: {e}")))?;
            return Ok(None);
        }
        tx.commit()
            .map_err(|e| SourceError::Other(format!("commit claim: {e}")))?;
        self.get_source(id)
    }

    /// Finds a post/repost source id by exact URL (manual `/enrich`).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn find_source_id_by_url(&self, url: &str) -> Result<Option<i64>, SourceError> {
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM sources
                 WHERE url = ?1 AND activity IN ('post', 'repost')
                 LIMIT 1",
                params![url.trim()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| SourceError::Other(format!("find by url: {e}")))?;
        Ok(id)
    }

    /// Finds any source id by exact URL (publisher `/ingest` upsert).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn find_any_source_id_by_url(&self, url: &str) -> Result<Option<i64>, SourceError> {
        Self::find_source_id_by_url_on(&self.conn, url)
    }

    /// Same as [`Self::find_any_source_id_by_url`] on an open connection.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn find_source_id_by_url_on(
        conn: &Connection,
        url: &str,
    ) -> Result<Option<i64>, SourceError> {
        let id: Option<i64> = conn
            .query_row(
                "SELECT id FROM sources WHERE url = ?1 LIMIT 1",
                params![url.trim()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| SourceError::Other(format!("find any by url: {e}")))?;
        Ok(id)
    }

    /// Deletes chunks for a source on an open connection.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn delete_chunks_for_source_on(
        conn: &Connection,
        source_id: i64,
    ) -> Result<(), SourceError> {
        conn.execute(
            "DELETE FROM chunks WHERE source_id = ?1",
            params![source_id],
        )
        .map_err(|e| SourceError::Other(format!("delete chunks: {e}")))?;
        Ok(())
    }

    /// Updates title/subject/body for a publisher URL ingest row.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn update_url_ingest_on(
        conn: &Connection,
        source_id: i64,
        subject: &str,
        title: &str,
        raw_text: &str,
    ) -> Result<(), SourceError> {
        let now = Local::now().to_rfc3339();
        conn.execute(
            "UPDATE sources
             SET subject = ?1, title = ?2, raw_text = ?3, created_at = ?4
             WHERE id = ?5",
            params![subject, title, raw_text, now, source_id],
        )
        .map_err(|e| SourceError::Other(format!("update url ingest: {e}")))?;
        Ok(())
    }

    /// Inserts a link stub or returns an existing post/repost row for manual Tor enrich.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn upsert_manual_enrich_source(&self, url: &str) -> Result<i64, SourceError> {
        let url = url.trim();
        if let Some(id) = self.find_source_id_by_url(url)? {
            self.conn
                .execute(
                    "UPDATE sources
                     SET enrich_status = 'pending',
                         enrich_after = NULL,
                         enrich_claimed_at = NULL
                     WHERE id = ?1
                       AND enrich_status IN ('ok', 'failed', 'none', 'pending')",
                    params![id],
                )
                .map_err(|e| SourceError::Other(format!("reset manual enrich: {e}")))?;
            return Ok(id);
        }
        let raw = format!("Post\n{url}");
        let now = Local::now().to_rfc3339();
        let id = self
            .insert_source(&InsertSource {
                kind: "personal_feed",
                activity: "post",
                subject: "x",
                title: "Post",
                url: Some(url),
                raw_text: &raw,
                occurred_at: Some(&now),
            })?
            .ok_or_else(|| {
                SourceError::Other("manual enrich insert conflict (retry)".to_string())
            })?;
        self.conn
            .execute(
                "UPDATE sources SET enrich_status = 'pending' WHERE id = ?1",
                params![id],
            )
            .map_err(|e| SourceError::Other(format!("set pending: {e}")))?;
        Ok(id)
    }

    /// Claims one source row by id for manual `/enrich` (same `in_flight` semantics as drip).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn claim_enrich_by_id(
        &self,
        source_id: i64,
        now_rfc3339: &str,
    ) -> Result<SourceRecord, SourceError> {
        let updated = self
            .conn
            .execute(
                "UPDATE sources
                 SET enrich_status = 'in_flight', enrich_claimed_at = ?1
                 WHERE id = ?2
                   AND enrich_status NOT IN ('in_flight', 'skip')
                   AND activity IN ('post', 'repost')",
                params![now_rfc3339, source_id],
            )
            .map_err(|e| SourceError::Other(format!("claim by id: {e}")))?;
        if updated == 0 {
            return Err(SourceError::Other(format!(
                "claim enrich: source {source_id} not claimable"
            )));
        }
        self.get_source(source_id)?
            .ok_or_else(|| SourceError::Other(format!("source {source_id} missing after claim")))
    }

    /// Commits in-place enrich success: text + subject + `ok` (chunks already replaced by caller).
    ///
    /// Does **not** change `title` (dedupe key includes title; weekly re-import must keep matching).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn complete_enrich(
        &self,
        source_id: i64,
        raw_text: &str,
        subject: &str,
        now_rfc3339: &str,
    ) -> Result<(), SourceError> {
        let n = self
            .conn
            .execute(
                "UPDATE sources
                 SET raw_text = ?1,
                     subject = ?2,
                     enrich_status = 'ok',
                     enriched_at = ?3,
                     enrich_after = NULL,
                     enrich_claimed_at = NULL
                 WHERE id = ?4 AND enrich_status = 'in_flight'",
                params![raw_text, subject, now_rfc3339, source_id],
            )
            .map_err(|e| SourceError::Other(format!("complete enrich: {e}")))?;
        if n == 0 {
            return Err(SourceError::Other(format!(
                "complete enrich: source {source_id} not in_flight"
            )));
        }
        Ok(())
    }

    /// Marks enrich failed with backoff `enrich_after`.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn fail_enrich(
        &self,
        source_id: i64,
        enrich_after_rfc3339: &str,
    ) -> Result<(), SourceError> {
        self.conn
            .execute(
                "UPDATE sources
                 SET enrich_status = 'failed',
                     enrich_after = ?1,
                     enrich_claimed_at = NULL
                 WHERE id = ?2 AND enrich_status = 'in_flight'",
                params![enrich_after_rfc3339, source_id],
            )
            .map_err(|e| SourceError::Other(format!("fail enrich: {e}")))?;
        Ok(())
    }

    /// Parks a source out of the drip queue (`skip`). Claim never selects `skip`.
    ///
    /// Use for deleted posts (`post_unavailable`) so they do not retry every drip.
    /// Manual refetch later: `UPDATE … SET enrich_status='pending', enrich_after=NULL`.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn skip_enrich(&self, source_id: i64) -> Result<(), SourceError> {
        let n = self
            .conn
            .execute(
                "UPDATE sources
                 SET enrich_status = 'skip',
                     enrich_after = NULL,
                     enrich_claimed_at = NULL
                 WHERE id = ?1 AND enrich_status = 'in_flight'",
                params![source_id],
            )
            .map_err(|e| SourceError::Other(format!("skip enrich: {e}")))?;
        if n == 0 {
            return Err(SourceError::Other(format!(
                "skip enrich: source {source_id} not in_flight"
            )));
        }
        Ok(())
    }

    /// Counts enrich statuses for post/repost rows (+ next due time).
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn enrich_status_counts(&self) -> Result<EnrichStatusCounts, SourceError> {
        let mut out = EnrichStatusCounts::default();
        let mut stmt = self
            .conn
            .prepare(
                "SELECT enrich_status, COUNT(*) FROM sources
                 WHERE activity IN ('post', 'repost')
                 GROUP BY enrich_status",
            )
            .map_err(|e| SourceError::Other(format!("prepare enrich counts: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.cast_unsigned(),
                ))
            })
            .map_err(|e| SourceError::Other(format!("query enrich counts: {e}")))?;
        for row in rows {
            let (status, n) = row.map_err(|e| SourceError::Other(format!("row enrich: {e}")))?;
            match status.as_str() {
                "pending" => out.pending = n,
                "in_flight" => out.in_flight = n,
                "ok" => out.ok = n,
                "failed" => out.failed = n,
                "skip" => out.skip = n,
                _ => out.none += n,
            }
        }
        out.next_enrich_after = self
            .conn
            .query_row(
                "SELECT enrich_after FROM sources
                 WHERE activity IN ('post', 'repost')
                   AND enrich_status IN ('pending', 'failed', 'none')
                   AND enrich_after IS NOT NULL
                 ORDER BY enrich_after ASC
                 LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| SourceError::Other(format!("next enrich_after: {e}")))?;
        Ok(out)
    }

    /// Deletes all sources and chunks (keeps Slack memory tables). For rebuild only.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    pub fn clear_corpus(&self) -> Result<(), SourceError> {
        self.conn
            .execute_batch("DELETE FROM chunks; DELETE FROM sources;")
            .map_err(|e| SourceError::Other(format!("clear corpus: {e}")))?;
        Ok(())
    }

    /// Test helper: force `in_flight` with a past claim timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Open`] on `SQLite` open/migrate failure, or [`SourceError::Other`] for query failures.
    #[cfg(test)]
    pub fn test_force_enrich_inflight(
        &self,
        source_id: i64,
        claimed_at: &str,
    ) -> Result<(), SourceError> {
        self.conn
            .execute(
                "UPDATE sources SET enrich_status = 'in_flight', enrich_claimed_at = ?1 WHERE id = ?2",
                params![claimed_at, source_id],
            )
            .map_err(|e| SourceError::Other(format!("test force inflight: {e}")))?;
        Ok(())
    }
}

/// Cosine similarity; 0.0 when either vector is empty or norms are zero.
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn insert_source_and_chunk_roundtrip() {
        let dir = TempDir::new().expect("tempdir");
        let db = SourceDb::open(dir.path().join("s.db")).expect("open");
        let sid = db
            .insert_source(&InsertSource {
                kind: "url",
                activity: "url",
                subject: "rust",
                title: "Rust async",
                url: Some("https://example.com/rust"),
                raw_text: "async await tokio",
                occurred_at: Some("2024-01-01T00:00:00"),
            })
            .expect("source")
            .expect("inserted");
        let emb = vec![1.0, 0.0, 0.0];
        db.insert_chunk(sid, "rust", "async await tokio", &emb)
            .expect("chunk");
        let got = db.get_chunk_candidates("rust", 10).expect("get");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "async await tokio");
        assert_eq!(got[0].embedding, emb);
        assert_eq!(db.source_count().expect("count"), 1);
        let full = db.get_source(sid).expect("get").expect("row");
        assert_eq!(full.activity, "url");
        assert_eq!(full.occurred_at.as_deref(), Some("2024-01-01T00:00:00"));
    }

    #[test]
    fn dedupe_skips_second_insert() {
        let dir = TempDir::new().expect("tempdir");
        let db = SourceDb::open(dir.path().join("s.db")).expect("open");
        let row = InsertSource {
            kind: "personal_feed",
            activity: "post",
            subject: "rust",
            title: "Hello",
            url: Some("https://li/1"),
            raw_text: "body",
            occurred_at: Some("2024-06-01T12:00:00"),
        };
        let first = db.insert_source(&row).expect("1").expect("id");
        let second = db.insert_source(&row).expect("2");
        assert!(second.is_none());
        assert_eq!(db.source_count().expect("count"), 1);
        assert!(db
            .source_exists(
                "post",
                Some("https://li/1"),
                Some("2024-06-01T12:00:00"),
                "Hello"
            )
            .expect("exists"));
        let _ = first;
    }

    #[test]
    fn manual_enrich_upsert_and_claim() {
        let dir = TempDir::new().expect("tempdir");
        let db = SourceDb::open(dir.path().join("s.db")).expect("open");
        let url = "https://www.linkedin.com/feed/update/urn:li:activity:upsert1";
        let id = db.upsert_manual_enrich_source(url).expect("upsert");
        let row = db.get_source(id).expect("get").expect("row");
        assert_eq!(row.enrich_status, "pending");
        assert_eq!(row.url.as_deref(), Some(url));
        let id2 = db.upsert_manual_enrich_source(url).expect("upsert2");
        assert_eq!(id, id2);
        let now = Local::now().to_rfc3339();
        let claimed = db.claim_enrich_by_id(id, &now).expect("claim");
        assert_eq!(claimed.enrich_status, "in_flight");
    }

    #[test]
    fn list_orders_by_occurred_at() {
        let dir = TempDir::new().expect("tempdir");
        let db = SourceDb::open(dir.path().join("s.db")).expect("open");
        db.insert_source(&InsertSource {
            kind: "personal_feed",
            activity: "post",
            subject: "x",
            title: "old",
            url: None,
            raw_text: "old text",
            occurred_at: Some("2020-01-01T00:00:00"),
        })
        .expect("old");
        db.insert_source(&InsertSource {
            kind: "personal_feed",
            activity: "post",
            subject: "x",
            title: "new",
            url: None,
            raw_text: "new text",
            occurred_at: Some("2025-01-01T00:00:00"),
        })
        .expect("new");
        let list = db
            .list_sources(&SourceListFilter {
                activity: "post".into(),
                limit: 10,
                preview_chars: 80,
                ..Default::default()
            })
            .expect("list");
        assert_eq!(list[0].title, "new");
        assert_eq!(list[1].title, "old");
        let counts = db.counts_by_activity().expect("counts");
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].count, 2);
    }

    #[test]
    fn purge_reactions_removes_rows() {
        let dir = TempDir::new().expect("tempdir");
        let db = SourceDb::open(dir.path().join("s.db")).expect("open");
        db.insert_source(&InsertSource {
            kind: "personal_feed",
            activity: "reaction",
            subject: "x",
            title: "Reaction LIKE",
            url: Some("https://li/r"),
            raw_text: "Reaction: LIKE\nhttps://li/r",
            occurred_at: Some("2024-01-01T00:00:00"),
        })
        .expect("reaction");
        db.insert_source(&InsertSource {
            kind: "personal_feed",
            activity: "post",
            subject: "x",
            title: "keep",
            url: Some("https://li/p"),
            raw_text: "body with enough text to stay",
            occurred_at: Some("2024-01-02T00:00:00"),
        })
        .expect("post");
        assert_eq!(db.source_count().expect("c"), 2);
        db.purge_reactions().expect("purge");
        assert_eq!(db.source_count().expect("c2"), 1);
    }

    #[test]
    fn enrich_claim_skips_ok() {
        let dir = TempDir::new().expect("tempdir");
        let db = SourceDb::open(dir.path().join("s.db")).expect("open");
        let sid = db
            .insert_source(&InsertSource {
                kind: "personal_feed",
                activity: "post",
                subject: "x",
                title: "stub",
                url: Some("https://li/1"),
                raw_text: "Post\nhttps://li/1",
                occurred_at: Some("2025-01-01T00:00:00"),
            })
            .expect("ins")
            .expect("id");
        db.ensure_enrich_stub_queue().expect("q");
        let now = "2026-01-01T00:00:00+00:00";
        let claimed = db.claim_next_enrich(now).expect("claim").expect("row");
        assert_eq!(claimed.id, sid);
        db.complete_enrich(sid, &"enriched body ".repeat(20), "rust", now)
            .expect("ok");
        let again = db.claim_next_enrich(now).expect("claim2");
        assert!(again.is_none());
    }

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-5);
    }
}

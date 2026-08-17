// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Durable `LinkedIn` URL scrape cache (separate from sources / runtime.db).
//!
//! Scraping != sources. Tor enrich writes here first so a sources wipe never
//! forces re-fetching the same activity URLs.

use crate::sqlite::open_configured;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Default scrape-cache path relative to the product root.
pub const DEFAULT_SCRAPE_CACHE_DB: &str = "sql/linkedin-scrape-cache.db";

const SCHEMA_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/scrape_pages_schema.sql"
));

/// Resolves scrape-cache path (`ITCY_SCRAPE_CACHE_DB`, else configured / default).
#[must_use]
pub fn resolve_scrape_cache_path(configured: &str) -> PathBuf {
    if let Ok(env_path) = std::env::var("ITCY_SCRAPE_CACHE_DB") {
        if !env_path.trim().is_empty() {
            return PathBuf::from(env_path);
        }
    }
    let raw = if configured.trim().is_empty() {
        DEFAULT_SCRAPE_CACHE_DB
    } else {
        configured.trim()
    };
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        return p;
    }
    crate::paths::product_join(p)
}

/// One cached scrape page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrapePage {
    pub url: String,
    pub fetched_at: String,
    pub http_status: Option<i64>,
    pub raw_html: String,
    pub extracted_text: String,
    pub ok: bool,
}

/// Errors from the scrape cache store.
#[derive(Debug, Error)]
pub enum ScrapeCacheError {
    #[error("scrape cache open: {0}")]
    Open(#[from] rusqlite::Error),
    #[error("scrape cache: {0}")]
    Other(String),
}

/// `SQLite` scrape-cache (URL → raw HTML + extracted text).
pub struct ScrapeCache {
    conn: Connection,
}

impl ScrapeCache {
    /// Opens (or creates) the scrape-cache DB.
    ///
    /// # Errors
    ///
    /// Returns [`ScrapeCacheError::Open`] on `SQLite` open failure, or [`ScrapeCacheError::Other`] for query failures.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ScrapeCacheError> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    ScrapeCacheError::Other(format!("create parent {}: {e}", parent.display()))
                })?;
            }
        }
        let conn = open_configured(path.as_ref())?;
        conn.execute_batch(SCHEMA_SQL)
            .map_err(|e| ScrapeCacheError::Other(format!("schema: {e}")))?;
        Ok(Self { conn })
    }

    /// Lookup by exact URL. Prefer `ok = true` pages for enrich apply.
    ///
    /// # Errors
    ///
    /// Returns [`ScrapeCacheError::Open`] on `SQLite` open failure, or [`ScrapeCacheError::Other`] for query failures.
    pub fn get(&self, url: &str) -> Result<Option<ScrapePage>, ScrapeCacheError> {
        let row = self
            .conn
            .query_row(
                "SELECT url, fetched_at, http_status, raw_html, extracted_text, ok
                 FROM scrape_pages WHERE url = ?1",
                params![url],
                |row| {
                    Ok(ScrapePage {
                        url: row.get(0)?,
                        fetched_at: row.get(1)?,
                        http_status: row.get(2)?,
                        raw_html: row.get(3)?,
                        extracted_text: row.get(4)?,
                        ok: row.get::<_, i64>(5)? != 0,
                    })
                },
            )
            .optional()
            .map_err(|e| ScrapeCacheError::Other(format!("get: {e}")))?;
        Ok(row)
    }

    /// Insert or replace a scrape result for `url`.
    ///
    /// # Errors
    ///
    /// Returns [`ScrapeCacheError::Open`] on `SQLite` open failure, or [`ScrapeCacheError::Other`] for query failures.
    pub fn upsert(&self, page: &ScrapePage) -> Result<(), ScrapeCacheError> {
        self.conn
            .execute(
                "INSERT INTO scrape_pages (url, fetched_at, http_status, raw_html, extracted_text, ok)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(url) DO UPDATE SET
                   fetched_at = excluded.fetched_at,
                   http_status = excluded.http_status,
                   raw_html = excluded.raw_html,
                   extracted_text = excluded.extracted_text,
                   ok = excluded.ok",
                params![
                    page.url,
                    page.fetched_at,
                    page.http_status,
                    page.raw_html,
                    page.extracted_text,
                    i64::from(page.ok),
                ],
            )
            .map_err(|e| ScrapeCacheError::Other(format!("upsert: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn upsert_and_get_roundtrip() {
        let dir = TempDir::new().expect("temp");
        let cache = ScrapeCache::open(dir.path().join("c.db")).expect("open");
        let page = ScrapePage {
            url: "https://www.linkedin.com/feed/update/urn:li:activity:1".into(),
            fetched_at: "2026-01-01T00:00:00Z".into(),
            http_status: Some(200),
            raw_html: "<html>body</html>".into(),
            extracted_text: "body text enough".into(),
            ok: true,
        };
        cache.upsert(&page).expect("upsert");
        let got = cache.get(&page.url).expect("get").expect("row");
        assert_eq!(got.extracted_text, "body text enough");
        assert!(got.ok);
    }
}

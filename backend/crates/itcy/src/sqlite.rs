// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Shared `SQLite` open settings for the single runtime DB file.

use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

/// Opens `path` with WAL journaling and a busy timeout.
///
/// Memory, sources, drafts, publish audit, RAG, and boot import share `runtime.db`
/// (`ITCy` app state; env `ITCY_STATE_DB` - generic name, not LinkedIn-only).
/// Without WAL, one writer (`LinkedIn` export import) blocks every other
/// connection and Slack draft/memory fail with `database is locked`.
/// Tor scrape results live in a separate `linkedin-scrape-cache.db`
/// (env `ITCY_SCRAPE_CACHE_DB`; not sources).
///
/// # Errors
///
/// Returns a [`rusqlite::Error`] when open or pragma setup fails.
pub fn open_configured(path: impl AsRef<Path>) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open(path.as_ref())?;
    configure(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.busy_timeout(Duration::from_secs(30))?;
    // journal_mode returns a row; execute_batch is fine for pragmas.
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
    Ok(())
}

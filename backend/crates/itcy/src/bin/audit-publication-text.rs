// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Read-only audit of existing publication text. JSON lines; never edits stored content.

use anyhow::{bail, Context, Result};
use itcy::llm::prose::normalize_publication_prose;
use itcy::sources::tweet_thread::{layout_x_thread, x_weighted_len};
use rusqlite::{Connection, OpenFlags};

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: audit-publication-text <state.db>")?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = conn.prepare(
        "SELECT draft_id, status, body FROM drafts
        WHERE draft_id LIKE 'DRAFT-%' OR draft_id LIKE 'TWEET-%'
        ORDER BY draft_id LIMIT 5001",
    )?;
    let mut rows = statement.query([])?;
    let mut inspected = 0;
    let mut affected = 0;
    while let Some(row) = rows.next()? {
        inspected += 1;
        if inspected > 5000 {
            bail!("audit exceeds 5000 records; narrow the query before continuing");
        }
        let id: String = row.get(0)?;
        let status: String = row.get(1)?;
        let body: String = row.get(2)?;
        if status == "building" || body.trim().is_empty() {
            continue;
        }
        let tweet = id.starts_with("TWEET-");
        let public = if tweet {
            itcy::publish::tweet_text_for_api(&body)
        } else {
            itcy::publish::linkedin_text_for_api(&body)
        };
        let normalized = normalize_publication_prose(&public);
        let normalization_required = public != normalized;
        let layout = tweet.then(|| layout_x_thread(&normalized));
        let layout_error = layout
            .as_ref()
            .and_then(|r| r.as_ref().err())
            .map(ToString::to_string);
        let validation_error = if tweet {
            itcy::publish::tweet_texts_for_api(&body)
                .err()
                .map(|e| e.to_string())
        } else if normalization_required {
            Some("punctuation/paragraph rework required".to_string())
        } else {
            None
        };
        if validation_error.is_some() {
            affected += 1;
            let lengths = layout
                .as_ref()
                .and_then(|r| r.as_ref().ok())
                .map(|parts| parts.iter().map(|p| x_weighted_len(p)).collect::<Vec<_>>());
            println!(
                "{}",
                serde_json::json!({
                    "id": id, "status": status, "reason": validation_error,
                    "normalization_required": normalization_required,
                    "normalized_layout_error": layout_error,
                    "normalized_part_lengths": lengths,
                })
            );
        }
    }
    eprintln!("Inspected {inspected} records; {affected} require review. Read-only; no content changed. Stored text is not proof of the text actually published.");
    Ok(())
}

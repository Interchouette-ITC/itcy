// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! `draft_status` - authoritative Draft row from `runtime.db` (never invent status).

use crate::bat::store::{status, DraftStore, StoredDraft};
use crate::llm::client::LlmError;
use std::path::Path;

/// Format one draft row for tools / freeform (status is ground truth).
#[must_use]
pub fn format_stored_draft_status(d: &StoredDraft) -> String {
    let pr = match (d.fork_pr_number, d.fork_pr_url.as_str()) {
        (Some(n), url) if !url.is_empty() => format!("#{n} {url}"),
        (Some(n), _) => format!("#{n}"),
        (None, url) if !url.is_empty() => url.to_string(),
        _ => "(none)".into(),
    };
    let next = match d.status.as_str() {
        status::PUBLISHED => {
            "Already a Post. Do not /accept, /rework, or /change_url.".into()
        }
        status::ACCEPTED => {
            "Waiting BAT (gRoussac Approve). /retry_bat if ship failed after BAT, or the webhook missed."
                .into()
        }
        status::OPEN => "/rework, /change_url, or /accept when ready.".into(),
        status::BUILDING => {
            "Still building. Wait for Slack, or /draft_about again if stalled.".into()
        }
        status::FAILED => "Failed mid-build. /draft_about for a new draft.".into(),
        other => format!("status=`{other}`."),
    };
    format!(
        "draft_id={}\nstatus={}\nsubject={}\nfork_pr={}\nnext={next}",
        d.draft_id, d.status, d.subject, pr
    )
}

/// Operator Slack reply when freeform asks where a Draft is (no LLM).
#[must_use]
pub fn operator_draft_status_reply(d: &StoredDraft) -> String {
    let pr_line = if d.fork_pr_url.is_empty() {
        String::new()
    } else {
        format!("\nPR: {}", d.fork_pr_url)
    };
    match d.status.as_str() {
        status::PUBLISHED => format!(
            "`{}` is already a **Post** (`published`).\nSubject: {}{pr_line}\n\
Do not `/accept` or rework.",
            d.draft_id, d.subject
        ),
        status::ACCEPTED => format!(
            "`{}` is **accepted** (fork Draft PR open; waiting gRoussac Approve / BAT).\n\
Subject: {}{pr_line}\n\
`/retry_bat {}` if ship failed after BAT, or Approve landed but the webhook missed.",
            d.draft_id, d.subject, d.draft_id
        ),
        status::OPEN => format!(
            "`{}` is **open** (ready for rework / accept).\nSubject: {}{pr_line}\n\
`/rework`, `/change_url`, or `/accept`.",
            d.draft_id, d.subject
        ),
        status::BUILDING => format!(
            "`{}` is still **building**. Wait for the Slack result, or `/draft_about` again if stalled.\n\
Subject: {}",
            d.draft_id, d.subject
        ),
        status::FAILED => format!(
            "`{}` **failed** mid-build. Run `/draft_about` for a new draft.\nSubject: {}",
            d.draft_id, d.subject
        ),
        other => format!(
            "`{}` status=`{other}`.\nSubject: {}{pr_line}",
            d.draft_id, d.subject
        ),
    }
}

/// Look up a Draft ID in the state DB.
///
/// # Errors
///
/// Returns [`LlmError::ToolProvider`] when `SQLite` fails.
pub fn lookup_draft_status(db_path: &Path, draft_id: &str) -> Result<String, LlmError> {
    let store = DraftStore::open(db_path)
        .map_err(|e| LlmError::ToolProvider(format!("draft_status: open db: {e}")))?;
    store
        .get(draft_id)
        .map_err(|e| LlmError::ToolProvider(format!("draft_status: get: {e}")))?
        .map_or_else(
            || {
                Ok(format!(
                    "draft_id={draft_id}\nstatus=missing\nNo row in runtime.db for this id."
                ))
            },
            |d| Ok(format_stored_draft_status(&d)),
        )
}

/// Parse tool args `{"draft_id":"DRAFT-…"}`.
///
/// # Errors
///
/// Returns [`LlmError::ToolProvider`] when the arg is missing.
pub fn parse_draft_id_arg(arguments: &str) -> Result<String, LlmError> {
    let v: serde_json::Value =
        serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!({}));
    let id = v
        .get("draft_id")
        .and_then(|q| q.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            LlmError::ToolProvider("draft_status requires {\"draft_id\": \"DRAFT-…\"}".into())
        })?;
    if !(id.starts_with("DRAFT-") || id.starts_with("TWEET-")) {
        return Err(LlmError::ToolProvider(format!(
            "draft_status: expected DRAFT-… or TWEET-… id, got `{id}`"
        )));
    }
    Ok(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bat::store::{stored_from_payload, DraftPayload, DraftStore};
    use tempfile::tempdir;

    #[test]
    fn published_reply_forbids_accept() {
        let d = StoredDraft {
            draft_id: "DRAFT-20260803-000028".into(),
            subject: "GitHub Models".into(),
            body: "x".into(),
            model: "m".into(),
            tokens_in: 1,
            tokens_out: 1,
            sources: vec![],
            link_options: vec![],
            research_pack: String::new(),
            status: status::PUBLISHED.into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            fork_pr_number: Some(1),
            fork_pr_url: "https://github.com/Interchouette/itcy-publications/pull/1".into(),
        };
        let reply = operator_draft_status_reply(&d);
        assert!(reply.contains("published"));
        assert!(reply.contains("Do not `/accept`"));
        assert!(!reply.to_ascii_lowercase().contains("pending"));
    }

    #[test]
    fn lookup_reads_sqlite() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("runtime.db");
        let store = DraftStore::open(&path).unwrap();
        let payload = DraftPayload {
            draft_id: "DRAFT-20990101-000001".into(),
            subject: "test".into(),
            body: "body".into(),
            model: "mock".into(),
            tokens_in: 1,
            tokens_out: 1,
            sources: vec![],
            link_options: vec![],
            research_pack: String::new(),
        };
        store.upsert(&stored_from_payload(payload)).unwrap();
        store
            .mark_status("DRAFT-20990101-000001", status::PUBLISHED)
            .unwrap();
        let out = lookup_draft_status(&path, "DRAFT-20990101-000001").unwrap();
        assert!(out.contains("status=published"));
        assert!(out.contains("Do not /accept"));
    }
}

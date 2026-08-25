// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Unified `/reply_comment`: `LinkedIn` (`CREPLY-`) and X (`XREPLY-`) reply drafts.
//!
//! `/accept` ships directly (no publications BAT PR).

use crate::bat::store::{status, stored_from_payload, DraftPayload, DraftStore};
use crate::bat::submit::ensure_open_for_edit;
use crate::llm::client::LlmMessage;
use crate::llm::router::{FailoverRouter, TaskKind};
use crate::llm::sanitize::sanitize_itcy_text;
use crate::prompts::{
    comment_reply_user_message, tweet_reply_user_message, COMMENT_REPLY_SYSTEM_CORE,
    TWEET_REPLY_SYSTEM_CORE,
};
use crate::publish::{
    activity_post_urn, parent_comment_urn, resolve_publish_mode_agile, ship_x_post,
    LinkedInMcpClient, PublishMode, XPublishRequest,
};
use crate::sources::linkedin_comment::{
    draft_comment_reply_parts, ensure_one_emoji, parse_linkedin_comment_url,
};
use crate::sources::tweet_thread::{fits_x_limit, x_weighted_len, X_CHAR_LIMIT};
use crate::sources::x_reply::{draft_tweet_reply_parts, parse_x_reply_url};
use crate::sqlite::open_configured;
use chrono::Local;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tracing::info;

const SEQ_SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS reply_code_seq (
    prefix TEXT PRIMARY KEY,
    next_ord INTEGER NOT NULL
);
";

/// Surface + parent context persisted in `research_pack` (JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyMeta {
    pub surface: String,
    pub url: String,
    pub author: String,
    /// Parent post (`LinkedIn`) or parent tweet (X).
    pub parent_body: String,
    /// `LinkedIn` comment text; empty on X (parent tweet is the target).
    #[serde(default)]
    pub target_body: String,
    #[serde(default)]
    pub activity_id: Option<String>,
    #[serde(default)]
    pub comment_id: Option<String>,
    #[serde(default)]
    pub status_id: Option<String>,
}

impl ReplyMeta {
    #[must_use]
    pub fn is_linkedin(&self) -> bool {
        self.surface.eq_ignore_ascii_case("linkedin")
    }

    #[must_use]
    pub fn is_x(&self) -> bool {
        self.surface.eq_ignore_ascii_case("x")
    }

    /// # Errors
    ///
    /// Returns an error when JSON is invalid.
    pub fn parse(raw: &str) -> Result<Self, String> {
        serde_json::from_str(raw.trim()).map_err(|e| format!("reply meta: {e}"))
    }

    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// True for comment-reply artefact ids.
#[must_use]
pub fn is_reply_id(id: &str) -> bool {
    id.starts_with("CREPLY-") || id.starts_with("XREPLY-")
}

/// Allocate `CREPLY-YYYYMMDD-NNNNNN` or `XREPLY-YYYYMMDD-NNNNNN`.
///
/// # Errors
///
/// Returns an operator-facing message on DB failure.
pub fn next_reply_id(db_path: &Path, prefix: &str) -> Result<String, String> {
    let prefix = prefix.trim().to_ascii_uppercase();
    if prefix != "CREPLY" && prefix != "XREPLY" {
        return Err(format!("unknown reply prefix `{prefix}`"));
    }
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create parent: {e}"))?;
        }
    }
    let conn = open_configured(db_path).map_err(|e| e.to_string())?;
    conn.execute_batch(SEQ_SCHEMA)
        .map_err(|e| format!("reply seq schema: {e}"))?;
    conn.execute(
        "INSERT OR IGNORE INTO reply_code_seq (prefix, next_ord) VALUES (?1, 0)",
        params![prefix],
    )
    .map_err(|e| format!("reply seq seed: {e}"))?;
    let ord: i64 = conn
        .query_row(
            "SELECT next_ord FROM reply_code_seq WHERE prefix = ?1",
            params![prefix],
            |r| r.get(0),
        )
        .map_err(|e| format!("reply seq read: {e}"))?;
    let next = ord.saturating_add(1);
    conn.execute(
        "UPDATE reply_code_seq SET next_ord = ?1 WHERE prefix = ?2",
        params![next, prefix],
    )
    .map_err(|e| format!("reply seq update: {e}"))?;
    let day = Local::now().format("%Y%m%d");
    Ok(format!("{prefix}-{day}-{next:06}"))
}

/// `/reply_comment <url>`: draft + save `CREPLY-` or `XREPLY-` (open).
///
/// # Errors
///
/// Returns an operator-facing message on bad URL, fetch, LLM, or store failure.
pub async fn create_reply_comment(
    llm: &Arc<FailoverRouter>,
    db_path: &Path,
    url: &str,
) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err(
            "usage: /reply_comment <linkedin activity https://… | x.com/…/status/…>".into(),
        );
    }
    match reply_prefix_for_url(url)? {
        "CREPLY" => create_linkedin_reply(llm, db_path, url).await,
        "XREPLY" => create_x_reply(llm, db_path, url).await,
        _ => unreachable!("reply_prefix_for_url only returns CREPLY|XREPLY"),
    }
}

/// Map operator URL → reply id prefix (`CREPLY` / `XREPLY`). Offline-testable.
///
/// # Errors
///
/// When the URL is neither `LinkedIn` nor X status.
pub fn reply_prefix_for_url(url: &str) -> Result<&'static str, String> {
    let lower = url.trim().to_ascii_lowercase();
    if lower.contains("linkedin.com/") {
        Ok("CREPLY")
    } else if lower.contains("x.com/")
        || lower.contains("twitter.com/")
        || url.trim().bytes().all(|b| b.is_ascii_digit())
    {
        Ok("XREPLY")
    } else {
        Err("URL must be a LinkedIn activity or an X status permalink".into())
    }
}

async fn create_linkedin_reply(
    llm: &Arc<FailoverRouter>,
    db_path: &Path,
    url: &str,
) -> Result<String, String> {
    let target = parse_linkedin_comment_url(url)?;
    let comment_id = target.comment_id.clone().ok_or_else(|| {
        "LinkedIn URL must include dashCommentUrn (threaded reply needs a parent comment id)"
            .to_string()
    })?;
    let (_t, ctx, reply) = draft_comment_reply_parts(llm, url).await?;
    let id = next_reply_id(db_path, "CREPLY")?;
    let meta = ReplyMeta {
        surface: "linkedin".into(),
        url: target.url.clone(),
        author: ctx.comment_author.clone(),
        parent_body: ctx.parent_post.clone(),
        target_body: ctx.comment_body.clone(),
        activity_id: Some(target.activity_id.clone()),
        comment_id: Some(comment_id),
        status_id: None,
    };
    save_open_reply(
        db_path,
        &SaveOpenReply {
            id: &id,
            subject: &format!("LI comment by {}", ctx.comment_author),
            reply: &reply,
            meta: &meta,
            model: "reply-comment/linkedin",
            tokens_in: 0,
            tokens_out: 0,
        },
    )?;
    Ok(format_create_slack(
        &id,
        "linkedin",
        &ctx.comment_author,
        &ctx.comment_body,
        &reply,
    ))
}

async fn create_x_reply(
    llm: &Arc<FailoverRouter>,
    db_path: &Path,
    url: &str,
) -> Result<String, String> {
    let (target, ctx, reply) = draft_tweet_reply_parts(llm, url).await?;
    let id = next_reply_id(db_path, "XREPLY")?;
    let meta = ReplyMeta {
        surface: "x".into(),
        url: target.url.clone(),
        author: ctx.author.clone(),
        parent_body: ctx.tweet_body.clone(),
        target_body: String::new(),
        activity_id: None,
        comment_id: None,
        status_id: Some(target.status_id),
    };
    save_open_reply(
        db_path,
        &SaveOpenReply {
            id: &id,
            subject: &format!("X status by {}", ctx.author),
            reply: &reply,
            meta: &meta,
            model: "reply-comment/x",
            tokens_in: 0,
            tokens_out: 0,
        },
    )?;
    Ok(format_create_slack(
        &id,
        "x",
        &ctx.author,
        &ctx.tweet_body,
        &reply,
    ))
}

struct SaveOpenReply<'a> {
    id: &'a str,
    subject: &'a str,
    reply: &'a str,
    meta: &'a ReplyMeta,
    model: &'a str,
    tokens_in: u32,
    tokens_out: u32,
}

fn save_open_reply(db_path: &Path, req: &SaveOpenReply<'_>) -> Result<(), String> {
    let store = DraftStore::open(db_path).map_err(|e| e.to_string())?;
    let body = crate::llm::disclosure::ensure_stored_disclosure(
        req.reply.trim(),
        req.model,
        req.tokens_in,
        req.tokens_out,
    );
    let row = stored_from_payload(DraftPayload {
        draft_id: req.id.to_string(),
        subject: req.subject.to_string(),
        body,
        model: req.model.to_string(),
        tokens_in: req.tokens_in,
        tokens_out: req.tokens_out,
        sources: vec![req.meta.url.clone()],
        link_options: Vec::new(),
        research_pack: req.meta.encode(),
    });
    store.upsert(&row).map_err(|e| e.to_string())?;
    info!(id = %req.id, surface = %req.meta.surface, "reply_comment: saved open");
    Ok(())
}

fn format_create_slack(id: &str, surface: &str, author: &str, parent: &str, reply: &str) -> String {
    format!(
        "Reply draft `{id}` ({surface}) saved (**open**).\n\n\
Parent ({author}): {parent}\n\n\
Reply:\n{reply}\n\n\
:point_right: Next:\n\n\
:pencil2: /rework {id} <instructions>\n\n\
:white_check_mark: /accept {id}",
        parent = parent.trim(),
        reply = reply.trim(),
    )
}

/// `/rework` on `CREPLY-` / `XREPLY-`.
///
/// # Errors
///
/// Returns an operator-facing message when the row is missing or the LLM fails.
pub async fn rework_reply_comment(
    llm: &Arc<FailoverRouter>,
    db_path: &Path,
    reply_id: &str,
    instructions: &str,
) -> Result<String, String> {
    if !is_reply_id(reply_id) {
        return Err(format!("`{reply_id}` is not a CREPLY- / XREPLY- id"));
    }
    let stored = ensure_open_for_edit(db_path, reply_id).map_err(|e| e.to_string())?;
    let meta = ReplyMeta::parse(&stored.research_pack)?;
    let prior = crate::publish::tweet_text_for_api(&stored.body);
    let prior = if prior.trim().is_empty() {
        crate::publish::linkedin_text_for_api(&stored.body)
    } else {
        prior
    };
    let reply = rewrite_reply(llm, &meta, prior.trim(), instructions.trim()).await?;
    let store = DraftStore::open(db_path).map_err(|e| e.to_string())?;
    let mut row = stored_from_payload(DraftPayload {
        draft_id: stored.draft_id.clone(),
        subject: stored.subject.clone(),
        body: crate::llm::disclosure::ensure_stored_disclosure(
            reply.trim(),
            "reply-comment/rework",
            0,
            0,
        ),
        model: "reply-comment/rework".into(),
        tokens_in: 0,
        tokens_out: 0,
        sources: stored.sources.clone(),
        link_options: Vec::new(),
        research_pack: meta.encode(),
    });
    row.fork_pr_number = stored.fork_pr_number;
    row.fork_pr_url.clone_from(&stored.fork_pr_url);
    row.created_at.clone_from(&stored.created_at);
    store.upsert(&row).map_err(|e| e.to_string())?;
    Ok(format!(
        "Reworked reply `{id}` saved (**open**).\n\n\
Reply:\n{reply}\n\n\
:point_right: Next:\n\n\
:pencil2: /rework {id} <instructions>\n\n\
:white_check_mark: /accept {id}",
        id = reply_id,
        reply = reply.trim(),
    ))
}

async fn rewrite_reply(
    llm: &Arc<FailoverRouter>,
    meta: &ReplyMeta,
    prior: &str,
    instructions: &str,
) -> Result<String, String> {
    let (system, base_user) = if meta.is_x() {
        (
            TWEET_REPLY_SYSTEM_CORE,
            tweet_reply_user_message(&meta.author, &meta.parent_body),
        )
    } else {
        let comment = if meta.target_body.trim().is_empty() {
            meta.parent_body.as_str()
        } else {
            meta.target_body.as_str()
        };
        (
            COMMENT_REPLY_SYSTEM_CORE,
            comment_reply_user_message(&meta.parent_body, &meta.author, comment),
        )
    };
    let user = format!(
        "{base_user}\n\nPrevious reply:\n{prior}\n\nOperator instructions (must follow):\n{instructions}\n\nWrite the reply only."
    );
    let messages = [LlmMessage::system(system), LlmMessage::user(user)];
    let (resp, _trace) = llm
        .complete(TaskKind::Freeform, &messages)
        .await
        .map_err(|e| format!("LLM failed: {e}"))?;
    let raw = resp.message.content.trim();
    if raw.is_empty() {
        return Err("LLM returned an empty reply".into());
    }
    let reply = ensure_one_emoji(&sanitize_itcy_text(raw));
    if meta.is_x() && !fits_x_limit(&reply) {
        return Err(format!(
            "LLM reply is {} weighted chars (X limit {X_CHAR_LIMIT})",
            x_weighted_len(&reply)
        ));
    }
    Ok(reply)
}

/// `/accept` on `CREPLY-` / `XREPLY-`: ship direct (no BAT PR).
///
/// # Errors
///
/// Returns an operator-facing message when the row is missing or ship fails.
pub async fn accept_reply_comment(db_path: &Path, reply_id: &str) -> Result<String, String> {
    if !is_reply_id(reply_id) {
        return Err(format!("`{reply_id}` is not a CREPLY- / XREPLY- id"));
    }
    let store = DraftStore::open(db_path).map_err(|e| e.to_string())?;
    let stored = store
        .get(reply_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("No reply `{reply_id}`"))?;
    match stored.status.as_str() {
        status::OPEN => {}
        status::PUBLISHED => {
            return Err(format!(
                "`{reply_id}` already published; use `/show {reply_id}`"
            ));
        }
        other => {
            return Err(format!(
                "`{reply_id}` status=`{other}` (need open to accept/ship)"
            ));
        }
    }
    let meta = ReplyMeta::parse(&stored.research_pack)?;
    let reply_text = if meta.is_x() {
        crate::publish::tweet_text_for_api(&stored.body)
    } else {
        crate::publish::linkedin_text_for_api(&stored.body)
    };
    let reply_text = reply_text.trim().to_string();
    if reply_text.is_empty() {
        return Err("empty reply body after chrome strip".into());
    }
    let ship_msg = if meta.is_x() {
        ship_x_reply(db_path, reply_id, &meta, &reply_text).await?
    } else {
        ship_linkedin_reply(reply_id, &meta, &reply_text).await?
    };
    let _ = store.mark_status(reply_id, status::PUBLISHED);
    Ok(ship_msg)
}

/// Build the X API ship request for an `XREPLY-` (thread reply, never quote).
fn x_reply_publish_request(
    reply_id: &str,
    meta: &ReplyMeta,
    reply_text: &str,
) -> Result<XPublishRequest, String> {
    let status_id = meta
        .status_id
        .clone()
        .or_else(|| parse_x_reply_url(&meta.url).ok().map(|t| t.status_id))
        .ok_or_else(|| "X reply meta missing status_id".to_string())?;
    if !fits_x_limit(reply_text) {
        return Err(format!(
            "reply is {} weighted chars (X limit {X_CHAR_LIMIT})",
            x_weighted_len(reply_text)
        ));
    }
    Ok(XPublishRequest {
        tweet_id: Some(reply_id.to_string()),
        pubs_pr_number: None,
        body: reply_text.to_string(),
        quote_tweet_id: None,
        in_reply_to_tweet_id: Some(status_id),
    })
}

async fn ship_x_reply(
    db_path: &Path,
    reply_id: &str,
    meta: &ReplyMeta,
    reply_text: &str,
) -> Result<String, String> {
    let request = x_reply_publish_request(reply_id, meta, reply_text)?;
    let result = ship_x_post(db_path, "playground", request, None)
        .await
        .map_err(|e| format!("X reply ship: {e}"))?;
    let shipped = result
        .linkedin_url
        .unwrap_or_else(|| "(no public URL)".into());
    Ok(format!(
        "Reply `{id}` shipped ({mode}) as X thread reply.\n\n\
Parent ({author}): {parent}\n\
Parent URL: {purl}\n\n\
Reply:\n{reply}\n\n\
Shipped: {shipped}\n\
{detail}",
        id = reply_id,
        mode = result.mode.as_str(),
        author = meta.author,
        parent = meta.parent_body.trim(),
        purl = meta.url,
        reply = reply_text,
        shipped = shipped,
        detail = result.detail.trim(),
    ))
}

async fn ship_linkedin_reply(
    reply_id: &str,
    meta: &ReplyMeta,
    reply_text: &str,
) -> Result<String, String> {
    let mode = resolve_publish_mode_agile("playground").map_err(|e| e.to_string())?;
    let activity_id = meta
        .activity_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "LinkedIn reply meta missing activity_id".to_string())?;
    let comment_id = meta
        .comment_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "LinkedIn reply meta missing comment_id".to_string())?;
    if mode == PublishMode::Playground {
        let post_urn = activity_post_urn(activity_id);
        let parent_urn = parent_comment_urn(activity_id, comment_id);
        return Ok(format!(
            "Reply `{id}` accepted (LinkedIn playground notice; not posted live).\n\n\
Parent ({author}): {parent}\n\n\
Reply:\n{reply}\n\n\
Would ship: post_urn={post_urn}\n\
parent_comment_urn={parent_urn}",
            id = reply_id,
            author = meta.author,
            parent = meta.parent_body.trim(),
            reply = reply_text,
        ));
    }
    let post_urn = activity_post_urn(activity_id);
    let parent_urn = parent_comment_urn(activity_id, comment_id);
    let client = LinkedInMcpClient::new();
    let mcp_detail = client
        .reply_to_comment(&post_urn, &parent_urn, reply_text)
        .await
        .map_err(|e| format!("LinkedIn MCP reply_to_comment: {e}"))?;
    Ok(format!(
        "Reply `{id}` shipped via LinkedIn MCP (production).\n\n\
Parent ({author}): {parent}\n\n\
Reply:\n{reply}\n\n\
MCP: {mcp}\n\
post_urn={post_urn}\n\
parent_comment_urn={parent_urn}",
        id = reply_id,
        author = meta.author,
        parent = meta.parent_body.trim(),
        reply = reply_text,
        mcp = mcp_detail.trim(),
    ))
}

/// Operator Next hints for reply rows (no `/change_url`, no BAT).
#[must_use]
pub fn reply_next_hints(id: &str, row_status: &str) -> String {
    match row_status {
        status::OPEN | status::BUILDING => format!(
            ":point_right: Next:\n\n\
:pencil2: /rework {id} <instructions>\n\n\
:white_check_mark: /accept {id}"
        ),
        status::PUBLISHED | status::FAILED => format!(
            ":point_right: Next:\n\n\
:mag: /show {id}\n\n\
:wastebasket: /delete {id}"
        ),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_id_helpers() {
        assert!(is_reply_id("CREPLY-20260825-000001"));
        assert!(is_reply_id("XREPLY-20260825-000001"));
        assert!(!is_reply_id("TWEET-20260825-000001"));
    }

    #[test]
    fn reply_comment_routes_x_status_to_xreply_prefix() {
        assert_eq!(
            reply_prefix_for_url("https://x.com/grok/status/2092338421779796069").unwrap(),
            "XREPLY"
        );
        assert_eq!(
            reply_prefix_for_url("2092338421779796069").unwrap(),
            "XREPLY"
        );
    }

    #[test]
    fn reply_comment_routes_linkedin_to_creply_prefix() {
        assert_eq!(
            reply_prefix_for_url(
                "https://www.linkedin.com/feed/update/urn:li:activity:123/?dashCommentUrn=x"
            )
            .unwrap(),
            "CREPLY"
        );
    }

    #[test]
    fn reply_next_hints_omit_change_url() {
        let h = reply_next_hints("XREPLY-20260825-000001", status::OPEN);
        assert!(h.contains("/accept XREPLY-20260825-000001"), "{h}");
        assert!(h.contains("/rework XREPLY-20260825-000001"), "{h}");
        assert!(!h.contains("/change_url"), "{h}");
        let c = reply_next_hints("CREPLY-20260825-000001", status::OPEN);
        assert!(c.contains("/accept CREPLY-20260825-000001"), "{c}");
        assert!(!c.contains("/change_url"), "{c}");
    }

    #[test]
    fn x_publish_request_sets_in_reply_to_for_xreply() {
        let meta = ReplyMeta {
            surface: "x".into(),
            url: "https://x.com/grok/status/2092338421779796069".into(),
            author: "Grok (@grok)".into(),
            parent_body: "hi".into(),
            target_body: String::new(),
            activity_id: None,
            comment_id: None,
            status_id: Some("2092338421779796069".into()),
        };
        let req = x_reply_publish_request("XREPLY-20260825-000001", &meta, "short reply 🦉")
            .expect("request");
        assert_eq!(req.tweet_id.as_deref(), Some("XREPLY-20260825-000001"));
        assert_eq!(
            req.in_reply_to_tweet_id.as_deref(),
            Some("2092338421779796069")
        );
        assert!(req.quote_tweet_id.is_none());
        assert!(req.pubs_pr_number.is_none());
    }

    #[test]
    fn meta_roundtrip() {
        let m = ReplyMeta {
            surface: "x".into(),
            url: "https://x.com/grok/status/1".into(),
            author: "Grok (@grok)".into(),
            parent_body: "hi".into(),
            target_body: String::new(),
            activity_id: None,
            comment_id: None,
            status_id: Some("1".into()),
        };
        let parsed = ReplyMeta::parse(&m.encode()).expect("parse");
        assert_eq!(parsed, m);
        assert!(parsed.is_x());
    }

    #[test]
    fn next_ids_monotonic() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("runtime.db");
        let a = next_reply_id(&db, "XREPLY").unwrap();
        let b = next_reply_id(&db, "XREPLY").unwrap();
        assert!(a.starts_with("XREPLY-"));
        assert_ne!(a, b);
        let c = next_reply_id(&db, "CREPLY").unwrap();
        assert!(c.starts_with("CREPLY-"));
    }
}

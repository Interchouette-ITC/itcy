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
    reply_rework_instruction_user_message, reply_rework_refresh_user_message,
    tweet_reply_rework_instruction_user_message, tweet_reply_rework_refresh_user_message,
    REPLY_REWORK_INSTRUCTION_SYSTEM_CORE, REPLY_REWORK_REFRESH_SYSTEM_CORE,
    TWEET_REPLY_REWORK_INSTRUCTION_SYSTEM_CORE, TWEET_REPLY_REWORK_REFRESH_SYSTEM_CORE,
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
    let (_t, ctx, reply, trace) = draft_comment_reply_parts(llm, url).await?;
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
    let model = format!("reply-comment/linkedin | {}", trace.model_label());
    save_open_reply(
        db_path,
        &SaveOpenReply {
            id: &id,
            subject: &format!("LI comment by {}", ctx.comment_author),
            reply: &reply,
            meta: &meta,
            model: &model,
            tokens_in: trace.prompt_tokens,
            tokens_out: trace.completion_tokens,
        },
    )?;
    Ok(format_create_slack(&CreateSlackArgs {
        id: &id,
        surface: "linkedin",
        author: &ctx.comment_author,
        parent: &ctx.comment_body,
        reply: &reply,
        model: &model,
        tokens_in: trace.prompt_tokens,
        tokens_out: trace.completion_tokens,
    }))
}

async fn create_x_reply(
    llm: &Arc<FailoverRouter>,
    db_path: &Path,
    url: &str,
) -> Result<String, String> {
    let (target, ctx, reply, trace) = draft_tweet_reply_parts(llm, url).await?;
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
    let model = format!("reply-comment/x | {}", trace.model_label());
    save_open_reply(
        db_path,
        &SaveOpenReply {
            id: &id,
            subject: &format!("X status by {}", ctx.author),
            reply: &reply,
            meta: &meta,
            model: &model,
            tokens_in: trace.prompt_tokens,
            tokens_out: trace.completion_tokens,
        },
    )?;
    Ok(format_create_slack(&CreateSlackArgs {
        id: &id,
        surface: "x",
        author: &ctx.author,
        parent: &ctx.tweet_body,
        reply: &reply,
        model: &model,
        tokens_in: trace.prompt_tokens,
        tokens_out: trace.completion_tokens,
    }))
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

struct CreateSlackArgs<'a> {
    id: &'a str,
    surface: &'a str,
    author: &'a str,
    parent: &'a str,
    reply: &'a str,
    model: &'a str,
    tokens_in: u32,
    tokens_out: u32,
}

fn format_create_slack(args: &CreateSlackArgs<'_>) -> String {
    let reply = crate::sources::draft_footer::slack_paste_safe_reply_body(args.reply);
    let disclosure = crate::llm::disclosure::format_disclosure_parts(
        args.model,
        args.tokens_in,
        args.tokens_out,
    );
    format!(
        "Reply draft `{id}` ({surface}) saved (**open**).\n\n\
Parent ({author}): {parent}\n\n\
Reply:\n{reply}\n\n\
{disclosure}\n\n\
:point_right: Next:\n\n\
:pencil2: /rework {id} <instructions>\n\n\
:white_check_mark: /accept {id}",
        id = args.id,
        surface = args.surface,
        author = args.author,
        parent = args.parent.trim(),
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
    let (reply, trace) = rewrite_reply(llm, &meta, prior.trim(), instructions.trim()).await?;
    let store = DraftStore::open(db_path).map_err(|e| e.to_string())?;
    let model = format!("reply-comment/rework | {}", trace.model_label());
    let mut row = stored_from_payload(DraftPayload {
        draft_id: stored.draft_id.clone(),
        subject: stored.subject.clone(),
        body: crate::llm::disclosure::ensure_stored_disclosure(
            reply.trim(),
            &model,
            trace.prompt_tokens,
            trace.completion_tokens,
        ),
        model,
        tokens_in: trace.prompt_tokens,
        tokens_out: trace.completion_tokens,
        sources: stored.sources.clone(),
        link_options: Vec::new(),
        research_pack: meta.encode(),
    });
    row.fork_pr_number = stored.fork_pr_number;
    row.fork_pr_url.clone_from(&stored.fork_pr_url);
    row.created_at.clone_from(&stored.created_at);
    store.upsert(&row).map_err(|e| e.to_string())?;
    let reply = crate::sources::draft_footer::slack_paste_safe_reply_body(&reply);
    let disclosure =
        crate::llm::disclosure::format_disclosure_parts(&row.model, row.tokens_in, row.tokens_out);
    Ok(format!(
        "Reworked reply `{reply_id}` saved (**open**).\n\n\
Reply:\n{reply}\n\n\
{disclosure}\n\n\
:point_right: Next:\n\n\
:pencil2: /rework {reply_id} <instructions>\n\n\
:white_check_mark: /accept {reply_id}"
    ))
}

async fn rewrite_reply(
    llm: &Arc<FailoverRouter>,
    meta: &ReplyMeta,
    prior: &str,
    instructions: &str,
) -> Result<(String, crate::llm::client::CompletionTrace), String> {
    use crate::sources::draft_footer::{
        classify_rework_mode, rework_collapsed_too_much, rework_verbatim_ban_phrases, ReworkMode,
    };
    let banned = rework_verbatim_ban_phrases(prior, instructions);
    match classify_rework_mode(instructions) {
        ReworkMode::Replace => apply_replacement_reply(meta, instructions, &banned),
        ReworkMode::Refresh => {
            rewrite_reply_llm(llm, meta, prior, instructions, &banned, true).await
        }
        ReworkMode::Instruction => {
            let (reply, trace) =
                rewrite_reply_llm(llm, meta, prior, instructions, &banned, false).await?;
            if rework_collapsed_too_much(prior, &reply, instructions) {
                return Err(
                    "rework collapsed into a stub; say what to change, or paste the full replacement reply"
                        .into(),
                );
            }
            Ok((reply, trace))
        }
    }
}

fn apply_replacement_reply(
    meta: &ReplyMeta,
    instructions: &str,
    banned: &[String],
) -> Result<(String, crate::llm::client::CompletionTrace), String> {
    use crate::sources::draft_footer::body_copies_rework_ban;
    let mut reply = ensure_one_emoji(&sanitize_itcy_text(
        crate::sources::draft_footer::rework_replacement_body(instructions),
    ));
    for phrase in banned {
        if phrase.chars().count() >= 24 {
            reply = reply.replace(phrase, "");
        }
    }
    reply = collapse_reply_ws(&reply);
    reply = ensure_one_emoji(&reply);
    if reply.trim().is_empty() {
        return Err("replacement draft emptied after scrub".into());
    }
    if body_copies_rework_ban(&reply, banned) {
        return Err(
            "replacement draft still contains old reply sentences; edit those out and /rework again"
                .into(),
        );
    }
    if meta.is_x() && !fits_x_limit(&reply) {
        return Err(format!(
            "replacement draft is {} weighted chars (X limit {X_CHAR_LIMIT})",
            x_weighted_len(&reply)
        ));
    }
    Ok((
        reply,
        crate::llm::client::CompletionTrace {
            provider: "operator".into(),
            model: "rework-replace".into(),
            prompt_tokens: 0,
            completion_tokens: 0,
        },
    ))
}

fn reply_rework_ban_block(banned: &[String]) -> String {
    if banned.is_empty() {
        return String::new();
    }
    let mut b = String::from(
        "\n\nHARD: Do NOT copy these phrases verbatim; rewrite them in new wording:\n",
    );
    for phrase in banned {
        b.push_str("- ");
        b.push_str(phrase);
        b.push('\n');
    }
    b
}

fn reply_rework_messages(
    meta: &ReplyMeta,
    prior: &str,
    instructions: &str,
    banned: &[String],
    refresh: bool,
) -> (String, String) {
    let ban_block = reply_rework_ban_block(banned);
    if meta.is_x() {
        if refresh {
            (
                TWEET_REPLY_REWORK_REFRESH_SYSTEM_CORE.to_string(),
                tweet_reply_rework_refresh_user_message(&meta.author, &meta.parent_body, prior),
            )
        } else {
            (
                TWEET_REPLY_REWORK_INSTRUCTION_SYSTEM_CORE.to_string(),
                tweet_reply_rework_instruction_user_message(
                    instructions,
                    &meta.author,
                    &meta.parent_body,
                    prior,
                    &ban_block,
                ),
            )
        }
    } else {
        let comment = if meta.target_body.trim().is_empty() {
            meta.parent_body.as_str()
        } else {
            meta.target_body.as_str()
        };
        if refresh {
            (
                REPLY_REWORK_REFRESH_SYSTEM_CORE.to_string(),
                reply_rework_refresh_user_message(&meta.parent_body, &meta.author, comment, prior),
            )
        } else {
            (
                REPLY_REWORK_INSTRUCTION_SYSTEM_CORE.to_string(),
                reply_rework_instruction_user_message(
                    instructions,
                    &meta.parent_body,
                    &meta.author,
                    comment,
                    prior,
                    &ban_block,
                ),
            )
        }
    }
}

async fn rewrite_reply_llm(
    llm: &Arc<FailoverRouter>,
    meta: &ReplyMeta,
    prior: &str,
    instructions: &str,
    banned: &[String],
    refresh: bool,
) -> Result<(String, crate::llm::client::CompletionTrace), String> {
    use crate::sources::draft_footer::body_copies_rework_ban;
    let (system, user) = reply_rework_messages(meta, prior, instructions, banned, refresh);
    let messages = [
        LlmMessage::system(system.as_str()),
        LlmMessage::user(user.clone()),
    ];
    // Draft route (same chef chain as writers), not Freeform short-reply chain.
    let (resp, mut trace) = llm
        .complete(TaskKind::Draft, &messages)
        .await
        .map_err(|e| format!("LLM failed: {e}"))?;
    let mut reply = ensure_one_emoji(&sanitize_itcy_text(resp.message.content.trim()));
    if !banned.is_empty() && body_copies_rework_ban(&reply, banned) {
        let retry_user = format!(
            "{user}\n\nRETRY: your previous draft still copied banned phrases. \
Rewrite those parts only; do not paste them again."
        );
        let retry_messages = [
            LlmMessage::system(system.as_str()),
            LlmMessage::user(retry_user),
        ];
        let (resp2, trace2) = llm
            .complete(TaskKind::Draft, &retry_messages)
            .await
            .map_err(|e| format!("LLM failed: {e}"))?;
        trace = trace2;
        reply = ensure_one_emoji(&sanitize_itcy_text(resp2.message.content.trim()));
    }
    if reply.trim().is_empty() {
        return Err("LLM returned an empty reply".into());
    }
    if !banned.is_empty() && body_copies_rework_ban(&reply, banned) {
        return Err("rework still copied the text you asked to reformulate; \
quote the exact sentences and try /rework again"
            .into());
    }
    if meta.is_x() && !fits_x_limit(&reply) {
        return Err(format!(
            "LLM reply is {} weighted chars (X limit {X_CHAR_LIMIT})",
            x_weighted_len(&reply)
        ));
    }
    Ok((reply, trace))
}

fn collapse_reply_ws(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" .", ".")
        .replace(" ,", ",")
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
        // Comment body only - never ship the AI disclosure footer to LinkedIn.
        crate::llm::disclosure::strip_trailing_disclosures(&crate::publish::linkedin_text_for_api(
            &stored.body,
        ))
        .trim()
        .to_string()
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
    let reply = crate::sources::draft_footer::slack_paste_safe_reply_body(reply_text);
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
    let reply = crate::sources::draft_footer::slack_paste_safe_reply_body(reply_text);
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
    fn format_create_slack_fences_reply_for_copy() {
        let msg = format_create_slack(&CreateSlackArgs {
            id: "CREPLY-20260826-000001",
            surface: "linkedin",
            author: "Toby",
            parent: "parent comment text",
            reply: "Visibility really does shift. :owl:",
            model: "reply-comment/linkedin | ollama/qwen3:8b",
            tokens_in: 1200,
            tokens_out: 48,
        });
        assert!(msg.contains("```\n"), "{msg}");
        assert!(msg.contains("🦉"), "{msg}");
        assert!(!msg.contains(":owl:"), "{msg}");
        assert!(msg.contains("Reply:\n```"), "{msg}");
        assert!(
            msg.contains("tokens in:1200 out:48"),
            "Slack create must show real token footer: {msg}"
        );
        assert!(
            msg.contains("Written by AI - ITCy"),
            "Slack create must show disclosure: {msg}"
        );
    }

    #[test]
    fn save_open_reply_stores_real_token_counts() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("runtime.db");
        let meta = ReplyMeta {
            surface: "linkedin".into(),
            url: "https://www.linkedin.com/feed/update/urn:li:activity:1/".into(),
            author: "DuckDB".into(),
            parent_body: "parent".into(),
            target_body: "comment".into(),
            activity_id: Some("1".into()),
            comment_id: Some("2".into()),
            status_id: None,
        };
        save_open_reply(
            &db,
            &SaveOpenReply {
                id: "CREPLY-20260828-000099",
                subject: "LI comment by DuckDB",
                reply: "You're welcome. 🦆",
                meta: &meta,
                model: "reply-comment/linkedin | ollama/qwen3:8b",
                tokens_in: 1842,
                tokens_out: 37,
            },
        )
        .expect("save");
        let store = DraftStore::open(&db).expect("open");
        let row = store
            .get("CREPLY-20260828-000099")
            .expect("get")
            .expect("row");
        assert_eq!(row.tokens_in, 1842);
        assert_eq!(row.tokens_out, 37);
        assert!(
            row.body.contains("tokens in:1842 out:37"),
            "stored body disclosure must carry tokens: {}",
            row.body
        );
        assert!(!row.body.contains("tokens in:0 out:0"));
    }

    #[test]
    fn linkedin_accept_ship_text_strips_disclosure() {
        let body = "You're welcome, DuckDB. 🦆\n\n\
Written by AI - ITCy - model reply-comment/linkedin | ollama/qwen3:8b - tokens in:1842 out:37";
        let shipped = crate::llm::disclosure::strip_trailing_disclosures(
            &crate::publish::linkedin_text_for_api(body),
        )
        .trim()
        .to_string();
        assert!(shipped.contains("You're welcome"));
        assert!(!shipped.contains("Written by AI"));
        assert!(!shipped.contains("tokens in:"));
    }

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

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! X status reply: fetch parent tweet, draft a short Slack reply, optional Brave/API ship.

use crate::llm::client::LlmMessage;
use crate::llm::router::{FailoverRouter, TaskKind};
use crate::llm::sanitize::sanitize_itcy_text;
use crate::prompts::{tweet_reply_user_message, TWEET_REPLY_SYSTEM_CORE};
use crate::publish::{ship_x_post, XPublishRequest};
use crate::sources::linkedin_comment::ensure_one_emoji;
use crate::sources::tweet_thread::{fits_x_limit, x_weighted_len, X_CHAR_LIMIT};
use crate::sources::twitter::{TwitterTool, TwitterToolError};
use crate::sources::url_hygiene::{is_x_status_url, x_status_id};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tracing::info;

/// Parsed X status URL for a threaded reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XReplyTarget {
    pub status_id: String,
    pub handle: Option<String>,
    pub url: String,
}

/// Parent tweet extracted for the reply writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XReplyContext {
    pub author: String,
    pub tweet_body: String,
}

/// Parse an X/Twitter status URL (or bare numeric id).
///
/// # Errors
///
/// Returns a short operator-facing message when the URL is not a status permalink.
pub fn parse_x_reply_url(raw: &str) -> Result<XReplyTarget, String> {
    let t = raw.trim().trim_end_matches(['.', ',', ')', ']']);
    if t.is_empty() {
        return Err(
            "usage: /accept_tweet_reply or /ship_tweet_reply <https://x.com/…/status/…>".into(),
        );
    }
    if t.bytes().all(|b| b.is_ascii_digit()) {
        let id = t.to_string();
        return Ok(XReplyTarget {
            status_id: id.clone(),
            handle: None,
            url: format!("https://x.com/i/web/status/{id}"),
        });
    }
    if !(t.starts_with("http://") || t.starts_with("https://")) {
        return Err("pass a full https:// X status URL (or a numeric status id)".into());
    }
    let lower = t.to_ascii_lowercase();
    if !(lower.contains("x.com/") || lower.contains("twitter.com/")) {
        return Err("URL must be on x.com or twitter.com".into());
    }
    let Some(status_id) = x_status_id(t) else {
        return Err("URL must include /status/<id>".into());
    };
    if !is_x_status_url(t) {
        return Err("URL must be an X status permalink".into());
    }
    let handle = handle_from_status_url(t);
    let url = handle.as_ref().map_or_else(
        || format!("https://x.com/i/web/status/{status_id}"),
        |h| format!("https://x.com/{h}/status/{status_id}"),
    );
    Ok(XReplyTarget {
        status_id,
        handle,
        url,
    })
}

fn handle_from_status_url(url: &str) -> Option<String> {
    let lower = url.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let path = rest
        .strip_prefix("x.com/")
        .or_else(|| rest.strip_prefix("twitter.com/"))?;
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let mut parts = path.split('/');
    let first = parts.next()?.trim();
    if first.is_empty() || first == "i" || first == "intent" {
        return None;
    }
    if parts.next()? != "status" {
        return None;
    }
    Some(first.to_string())
}

/// Fetch parent tweet + draft a short reply for Slack (no ship).
///
/// # Errors
///
/// Returns an operator-facing message on bad URL, fetch miss, or LLM failure.
pub async fn draft_tweet_reply_for_slack(
    llm: &Arc<FailoverRouter>,
    url: &str,
) -> Result<String, String> {
    let (_target, ctx, reply, _trace) = draft_tweet_reply_parts(llm, url).await?;
    Ok(format_slack_draft(&ctx, &reply))
}

/// Draft + ship a threaded reply under the parent status (Brave or X API).
///
/// # Errors
///
/// Returns an operator-facing message on draft or ship failure.
pub async fn ship_tweet_reply(
    llm: &Arc<FailoverRouter>,
    state_db_path: impl AsRef<Path>,
    url: &str,
) -> Result<String, String> {
    let (target, ctx, reply, _trace) = draft_tweet_reply_parts(llm, url).await?;
    if !fits_x_limit(&reply) {
        return Err(format!(
            "reply is {} weighted chars (X limit {X_CHAR_LIMIT}); shorten with instructions in chat",
            x_weighted_len(&reply)
        ));
    }
    let request = XPublishRequest {
        tweet_id: None,
        pubs_pr_number: None,
        body: reply.clone(),
        quote_tweet_id: None,
        in_reply_to_tweet_id: Some(target.status_id.clone()),
    };
    let result = ship_x_post(state_db_path, "playground", request, None)
        .await
        .map_err(|e| format!("X reply ship: {e}"))?;
    let shipped = result
        .linkedin_url
        .unwrap_or_else(|| "(no public URL)".into());
    Ok(format!(
        "Tweet reply shipped ({mode})\n\n\
Parent ({author}): {tweet}\n\
Parent URL: {parent}\n\n\
Reply:\n{reply}\n\n\
Shipped: {shipped}\n\
{detail}",
        mode = result.mode.as_str(),
        author = ctx.author,
        tweet = ctx.tweet_body,
        parent = target.url,
        reply = reply.trim(),
        shipped = shipped,
        detail = result.detail.trim(),
    ))
}

/// Fetch parent + draft reply text (shared by Slack create and ship).
///
/// # Errors
///
/// Returns an operator-facing message on bad URL, fetch miss, or LLM failure.
pub async fn draft_tweet_reply_parts(
    llm: &Arc<FailoverRouter>,
    url: &str,
) -> Result<
    (
        XReplyTarget,
        XReplyContext,
        String,
        crate::llm::client::CompletionTrace,
    ),
    String,
> {
    let target = parse_x_reply_url(url)?;
    info!(
        status_id = %target.status_id,
        handle = ?target.handle,
        "x_reply: fetch start"
    );
    let ctx = fetch_x_reply_context(&target).await?;
    let (reply, trace) = generate_reply(llm, &ctx).await?;
    Ok((target, ctx, reply, trace))
}

async fn fetch_x_reply_context(target: &XReplyTarget) -> Result<XReplyContext, String> {
    if let Ok(tool) = TwitterTool::from_disk() {
        match tool.lookup_status_with_author(&target.status_id).await {
            Ok(ctx) => return Ok(ctx),
            Err(e) => {
                info!(error = %e, "x_reply: bearer lookup missed; trying Brave");
            }
        }
    }
    fetch_x_reply_via_brave(target).await
}

async fn fetch_x_reply_via_brave(target: &XReplyTarget) -> Result<XReplyContext, String> {
    let script = resolve_fetch_status_cmd().ok_or_else(|| {
        "scripts/fetch-twitter-status.sh not found (Brave status fetch)".to_string()
    })?;
    let arg = if target.handle.is_some() {
        target.url.clone()
    } else {
        target.status_id.clone()
    };
    let out = Command::new(&script)
        .arg(&arg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Brave status fetch spawn: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        return Err(format!(
            "Brave status fetch failed (exit {:?}): {stderr} {stdout}",
            out.status.code()
        ));
    }
    let parsed: StatusFetchJson = serde_json::from_str(&stdout)
        .map_err(|e| format!("Brave status JSON parse: {e}; stdout={stdout}; stderr={stderr}"))?;
    if !parsed.ok {
        return Err(format!(
            "Brave status fetch refused: {}",
            parsed.detail.unwrap_or(stdout)
        ));
    }
    let text = parsed.text.unwrap_or_default().trim().to_string();
    if text.is_empty() {
        return Err("Brave status fetch returned empty tweet text".into());
    }
    let author = parsed
        .author
        .filter(|s| !s.trim().is_empty())
        .or_else(|| target.handle.as_ref().map(|h| format!("@{h}")))
        .unwrap_or_else(|| "unknown".into());
    Ok(XReplyContext {
        author,
        tweet_body: text,
    })
}

fn resolve_fetch_status_cmd() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("ITCY_TWITTER_STATUS_CMD") {
        let p = PathBuf::from(raw.trim());
        if p.is_file() {
            return Some(p);
        }
    }
    [
        PathBuf::from("scripts/fetch-twitter-status.sh"),
        PathBuf::from("../scripts/fetch-twitter-status.sh"),
        crate::paths::product_join("scripts/fetch-twitter-status.sh"),
    ]
    .into_iter()
    .find(|c| c.is_file())
}

#[derive(Debug, Deserialize)]
struct StatusFetchJson {
    ok: bool,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    detail: Option<String>,
}

async fn generate_reply(
    llm: &Arc<FailoverRouter>,
    ctx: &XReplyContext,
) -> Result<(String, crate::llm::client::CompletionTrace), String> {
    let user = tweet_reply_user_message(&ctx.author, &ctx.tweet_body);
    let messages = [
        LlmMessage::system(TWEET_REPLY_SYSTEM_CORE),
        LlmMessage::user(user),
    ];
    let (resp, trace) = llm
        .complete(TaskKind::Freeform, &messages)
        .await
        .map_err(|e| format!("LLM failed: {e}"))?;
    let raw = resp.message.content.trim();
    if raw.is_empty() {
        return Err("LLM returned an empty reply".into());
    }
    let reply = ensure_one_emoji(&sanitize_itcy_text(raw));
    if !fits_x_limit(&reply) {
        return Err(format!(
            "LLM reply is {} weighted chars (X limit {X_CHAR_LIMIT})",
            x_weighted_len(&reply)
        ));
    }
    Ok((reply, trace))
}

fn format_slack_draft(ctx: &XReplyContext, reply: &str) -> String {
    format!(
        "Tweet reply draft (not shipped)\n\n\
Parent ({author}): {tweet}\n\n\
Reply:\n{reply}",
        author = ctx.author,
        tweet = ctx.tweet_body,
        reply = reply.trim()
    )
}

impl TwitterTool {
    /// Lookup status text + author for reply drafting.
    ///
    /// # Errors
    ///
    /// Returns [`TwitterToolError`] on HTTP/API failure or missing Bearer.
    pub async fn lookup_status_with_author(
        &self,
        status_id: &str,
    ) -> Result<XReplyContext, TwitterToolError> {
        let id = status_id.trim();
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(TwitterToolError::Other("invalid status id".into()));
        }
        let Some(bearer) = self.creds().bearer.as_ref().filter(|s| !s.is_empty()) else {
            return Err(TwitterToolError::Other(
                "no bearer for status lookup".into(),
            ));
        };
        let client = reqwest::Client::new();
        let url = format!(
            "{base}/tweets/{id}?tweet.fields=created_at,text,author_id&expansions=author_id&user.fields=username,name",
            base = crate::sources::url_hygiene::TWITTER_API_V2_BASE,
            id = id
        );
        let resp = client
            .get(&url)
            .bearer_auth(bearer)
            .send()
            .await
            .map_err(|e| TwitterToolError::Other(format!("lookup request: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(TwitterToolError::Other(format!(
                "lookup HTTP {status}: {}",
                body.chars().take(160).collect::<String>()
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| TwitterToolError::Other(format!("lookup json: {e}")))?;
        let data = body
            .get("data")
            .ok_or_else(|| TwitterToolError::Other("lookup missing data".into()))?;
        let text = data
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if text.is_empty() {
            return Err(TwitterToolError::Other("lookup empty tweet text".into()));
        }
        let author_id = data.get("author_id").and_then(|v| v.as_str());
        let author = author_from_includes(&body, author_id).unwrap_or_else(|| "unknown".into());
        Ok(XReplyContext {
            author,
            tweet_body: text,
        })
    }
}

fn author_from_includes(body: &serde_json::Value, author_id: Option<&str>) -> Option<String> {
    let users = body.pointer("/includes/users")?.as_array()?;
    let user = if let Some(aid) = author_id {
        users
            .iter()
            .find(|u| u.get("id").and_then(|v| v.as_str()) == Some(aid))?
    } else {
        users.first()?
    };
    let username = user.get("username").and_then(|v| v.as_str()).unwrap_or("");
    let name = user.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if !username.is_empty() && !name.is_empty() {
        Some(format!("{name} (@{username})"))
    } else if !username.is_empty() {
        Some(format!("@{username}"))
    } else if !name.is_empty() {
        Some(name.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_url_with_handle() {
        let t = parse_x_reply_url("https://x.com/grok/status/2092338421779796069").expect("parse");
        assert_eq!(t.status_id, "2092338421779796069");
        assert_eq!(t.handle.as_deref(), Some("grok"));
        assert!(t.url.contains("/grok/status/"));
    }

    #[test]
    fn parse_i_web_status() {
        let t = parse_x_reply_url("https://x.com/i/web/status/2092338421779796069").expect("parse");
        assert_eq!(t.status_id, "2092338421779796069");
        assert!(t.handle.is_none());
    }

    #[test]
    fn parse_bare_id() {
        let t = parse_x_reply_url("2092338421779796069").expect("parse");
        assert_eq!(t.status_id, "2092338421779796069");
    }

    #[test]
    fn parse_rejects_non_x() {
        assert!(parse_x_reply_url("https://linkedin.com/feed").is_err());
        assert!(parse_x_reply_url("https://x.com/grok").is_err());
    }

    #[test]
    fn author_from_api_includes() {
        let body = serde_json::json!({
            "includes": {
                "users": [{
                    "id": "1",
                    "name": "Grok",
                    "username": "grok"
                }]
            }
        });
        assert_eq!(
            author_from_includes(&body, Some("1")).as_deref(),
            Some("Grok (@grok)")
        );
    }
}

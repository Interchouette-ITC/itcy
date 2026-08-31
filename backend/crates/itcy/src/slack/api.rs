// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Thin Slack Web API: `chat.postMessage`.

use crate::sources::digest::{with_item_grey_bars, DigestSlackPost};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use tracing::warn;

const CHAT_POST_MESSAGE_URL: &str = "https://slack.com/api/chat.postMessage";
/// Slack allows ~1 `chat.postMessage` per second per channel; stay under that.
const POST_GAP: Duration = Duration::from_millis(1100);
const RATE_LIMIT_MAX_ATTEMPTS: u32 = 6;
const RATE_LIMIT_DEFAULT_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Deserialize)]
struct PostMessageResponse {
    ok: bool,
    error: Option<String>,
    ts: Option<String>,
}

/// Posts plain text to a channel (or DM) using the bot token.
///
/// # Errors
///
/// Returns `Err(String)` with an operator-facing message when validation or lookup fails.
pub async fn post_message(bot_token: &str, channel: &str, text: &str) -> Result<(), String> {
    // Operator drafts/tweets carry cite URLs; Slack unfurls look like extra body text.
    post_message_ex(bot_token, channel, text, None, false)
        .await
        .map(|_| ())
}

/// Posts `text`; optional Slack thread parent; returns message `ts`.
///
/// Retries on `ratelimited` using `Retry-After` when present.
///
/// # Errors
///
/// Returns `Err(String)` when the Web API rejects the post.
pub async fn post_message_ex(
    bot_token: &str,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
    unfurl_links: bool,
) -> Result<String, String> {
    let client = reqwest::Client::new();
    let mut body = json!({
        "channel": channel,
        "text": text,
        "unfurl_links": unfurl_links,
        "unfurl_media": unfurl_links,
    });
    if let Some(ts) = thread_ts {
        body["thread_ts"] = json!(ts);
    }

    let mut last_err = String::from("ratelimited");
    for attempt in 1..=RATE_LIMIT_MAX_ATTEMPTS {
        let resp = client
            .post(CHAT_POST_MESSAGE_URL)
            .bearer_auth(bot_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("chat.postMessage request: {e}"))?;

        let status = resp.status();
        let retry_after = retry_after_secs(resp.headers());
        let parsed: PostMessageResponse = resp
            .json()
            .await
            .map_err(|e| format!("chat.postMessage parse: {e}"))?;

        if parsed.ok {
            return parsed
                .ts
                .filter(|t| !t.is_empty())
                .ok_or_else(|| "chat.postMessage ok but missing ts".into());
        }

        let err = parsed
            .error
            .unwrap_or_else(|| format!("chat.postMessage failed (http {status})"));
        if err != "ratelimited" || attempt == RATE_LIMIT_MAX_ATTEMPTS {
            return Err(err);
        }
        last_err = err;
        let wait = retry_after
            .map_or(RATE_LIMIT_DEFAULT_WAIT, Duration::from_secs)
            .max(Duration::from_secs(1));
        warn!(
            attempt,
            wait_secs = wait.as_secs(),
            "slack: chat.postMessage ratelimited; retrying"
        );
        tokio::time::sleep(wait).await;
    }
    Err(last_err)
}

fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

/// Ship notice to `#daily-digest` (and operator channel for playground `LinkedIn` paste).
pub async fn post_ship_notice(post_id: &str, detail: &str) {
    if post_id.starts_with("POST-") && crate::bat::github::is_playground_mode() {
        let text = playground_linkedin_ship_slack_text(post_id, detail);
        post_ship_slack(&text, true).await;
        return;
    }
    if skip_playground_ship_notice(post_id) {
        return;
    }
    post_ship_slack(&crate::sources::format_ship_notice(post_id, detail), false).await;
}

/// X playground soft-ship stays quiet (no Brave post); `LinkedIn` playground must notify paste.
fn skip_playground_ship_notice(post_id: &str) -> bool {
    if post_id.starts_with("XPOST-") {
        return crate::bat::github::is_x_playground_mode();
    }
    false
}

fn playground_linkedin_ship_slack_text(post_id: &str, detail: &str) -> String {
    let db_path = std::env::var("ITCY_STATE_DB")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map_or_else(
            || crate::paths::product_join("sql/runtime.db"),
            std::path::PathBuf::from,
        );
    let draft_id = crate::bat::pack::post_id_to_draft_id(post_id)
        .unwrap_or_else(|| format!("DRAFT-{post_id}"));
    if let Ok(store) = crate::bat::store::DraftStore::open(&db_path) {
        if let Ok(Some(row)) = store.get(&draft_id) {
            let paste = crate::sources::draft_footer::linkedin_manual_paste_message(
                &row.body,
                &row.model,
                row.tokens_in,
                row.tokens_out,
            );
            return crate::sources::format_playground_linkedin_ship_notice(post_id, &paste, detail);
        }
    }
    crate::sources::format_playground_linkedin_ship_notice(
        post_id,
        "(could not load draft body for paste; use /show)",
        detail,
    )
}

/// Ship failure to digest + operator channel.
pub async fn post_ship_fail(post_id: &str, error: &str) {
    post_ship_slack(&crate::sources::format_ship_fail(post_id, error), true).await;
}

/// BAT promote/ship failure after Approve (operator channel + digest).
pub async fn post_bat_fail(pr_number: u64, error: &str) {
    post_ship_slack(&crate::sources::format_bat_fail(pr_number, error), true).await;
}

async fn post_ship_slack(text: &str, also_operator: bool) {
    let token = env_nonempty("SLACK_BOT_TOKEN");
    let Some(token) = token else {
        return;
    };
    let mut channels = Vec::new();
    if let Some(ch) = env_nonempty("SLACK_DAILY_DIGEST_CHANNEL_ID") {
        channels.push(ch);
    }
    if also_operator {
        if let Some(ch) = env_nonempty("SLACK_ITCY_CHANNEL_ID") {
            if !channels.contains(&ch) {
                channels.push(ch);
            }
        }
    }
    for channel in channels {
        if let Err(e) = post_message(&token, &channel, text).await {
            tracing::warn!(error = %e, "publish: ship Slack post failed");
        }
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn post_thread(
    bot_token: &str,
    channel: &str,
    title: &str,
    items: &[String],
) -> Result<(), String> {
    if items.is_empty() {
        return Ok(());
    }
    let parent_ts = post_message_ex(bot_token, channel, title, None, false).await?;
    for text in with_item_grey_bars(items) {
        tokio::time::sleep(POST_GAP).await;
        let unfurl = text.contains("https://");
        post_message_ex(bot_token, channel, &text, Some(&parent_ts), unfurl).await?;
    }
    Ok(())
}

/// One header message, then each digest propose item in the thread (avoids multi-fence mega-posts).
///
/// # Errors
///
/// Returns after the first failed post; earlier messages may already be visible.
pub async fn post_propose_batch(
    bot_token: &str,
    channel: &str,
    header: &str,
    items: &[String],
) -> Result<(), String> {
    post_thread(bot_token, channel, header, items).await
}

/// Overview in-channel, then Press, For you, Following, Twitter search, Interchouette.
///
/// # Errors
///
/// Returns after the first failed post; earlier messages may already be visible.
pub async fn post_digest_channel(
    bot_token: &str,
    channel: &str,
    post: &DigestSlackPost,
) -> Result<(), String> {
    post_message_ex(bot_token, channel, &post.overview, None, false).await?;
    tokio::time::sleep(POST_GAP).await;
    post_thread(bot_token, channel, &post.press_title, &post.press_items).await?;
    tokio::time::sleep(POST_GAP).await;
    post_thread(bot_token, channel, &post.for_you_title, &post.for_you_items).await?;
    tokio::time::sleep(POST_GAP).await;
    post_thread(
        bot_token,
        channel,
        &post.following_title,
        &post.following_items,
    )
    .await?;
    tokio::time::sleep(POST_GAP).await;
    post_thread(bot_token, channel, &post.twitter_title, &post.twitter_items).await?;
    tokio::time::sleep(POST_GAP).await;
    post_thread(bot_token, channel, &post.itc_title, &post.itc_items).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn linkedin_post_playground_is_not_skipped_for_ship_notice() {
        // Regression DRAFT-20260831-000136: #59 skipped playground POST notices so Slack
        // never confirmed BAT after Approve (only the static /accept "waiting Approve" line).
        assert!(!super::skip_playground_ship_notice("POST-20260831-000136"));
    }

    #[test]
    fn xpost_playground_may_skip_ship_notice() {
        // X soft-ship stays quiet when x_publish_mode is playground.
        let _ = super::skip_playground_ship_notice("XPOST-1");
    }
}

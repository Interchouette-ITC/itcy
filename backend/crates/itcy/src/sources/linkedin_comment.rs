// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! `LinkedIn` comment URL parse, Tor guest fetch, and short Slack reply draft (no BAT).

use crate::llm::client::LlmMessage;
use crate::llm::router::{FailoverRouter, TaskKind};
use crate::llm::sanitize::{count_emoji, expand_emoji_shortcodes, sanitize_itcy_text};
use crate::prompts::{comment_reply_user_message, COMMENT_REPLY_SYSTEM_CORE};
use crate::sources::enrich::{TorPageFetcher, TorSocksFetcher, DEFAULT_TOR_SOCKS};
use crate::sources::html::html_to_text;
use serde_json::Value as JsonValue;
use std::sync::Arc;
use tracing::info;

/// Parsed `LinkedIn` feed-update URL targeting a post and optional comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedInCommentTarget {
    pub activity_id: String,
    pub comment_id: Option<String>,
    pub url: String,
}

/// Parent post + comment extracted from a browsed page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedInCommentContext {
    /// Full parent post body (not truncated).
    pub parent_post: String,
    pub comment_author: String,
    pub comment_body: String,
}

/// Parse a `LinkedIn` activity URL (optional `dashCommentUrn` / `fsd_comment`).
///
/// # Errors
///
/// Returns a short operator-facing message when the URL is not a feed update.
pub fn parse_linkedin_comment_url(raw: &str) -> Result<LinkedInCommentTarget, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(
            "usage: /accept_comment_reply or /ship_comment_reply <linkedin activity https://…>"
                .into(),
        );
    }
    if !(t.starts_with("http://") || t.starts_with("https://")) {
        return Err("pass a full https:// LinkedIn URL".into());
    }
    let lower = t.to_ascii_lowercase();
    if !lower.contains("linkedin.com/") {
        return Err("URL must be on linkedin.com".into());
    }
    let decoded = percent_decode_lite(t);
    let activity_id = extract_activity_id(&decoded)
        .ok_or_else(|| "URL must include urn:li:activity:<id>".to_string())?;
    let comment_id = extract_query_value(t, "dashCommentUrn")
        .or_else(|| extract_query_value(t, "commentUrn"))
        .and_then(|v| parse_fsd_comment_id(&v))
        .or_else(|| parse_fsd_comment_id(&decoded));

    Ok(LinkedInCommentTarget {
        activity_id,
        comment_id,
        url: t.to_string(),
    })
}

fn extract_activity_id(s: &str) -> Option<String> {
    let marker = "urn:li:activity:";
    let start = s.find(marker)?;
    let rest = &s[start + marker.len()..];
    let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn extract_query_value(url: &str, key: &str) -> Option<String> {
    let q_start = url.find('?')?;
    let query = &url[q_start + 1..];
    let query = query.split('#').next().unwrap_or(query);
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        if k == key {
            return Some(percent_decode_lite(it.next().unwrap_or("")));
        }
    }
    None
}

fn percent_decode_lite(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(a), Some(b)) = (hi, lo) {
                if let Ok(byte) = u8::try_from((a << 4) | b) {
                    out.push(char::from(byte));
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn parse_fsd_comment_id(raw: &str) -> Option<String> {
    let decoded = percent_decode_lite(raw);
    // urn:li:fsd_comment:(7496664683873992704,urn:li:activity:7496571584363741184)
    let start = decoded.find("fsd_comment:(")?;
    let rest = &decoded[start + "fsd_comment:(".len()..];
    let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Pull full parent post + comment from guest HTML (JSON-LD first, then visible text).
#[must_use]
pub fn extract_comment_context_from_html(html: &str) -> Option<LinkedInCommentContext> {
    if let Some(ctx) = extract_from_json_ld(html) {
        return Some(ctx);
    }
    // html_to_text collapses newlines; re-break on common guest chrome phrases for line extract.
    let flat = html_to_text(html);
    let lined = rebreak_guest_text(&flat);
    extract_comment_context(&lined)
}

fn extract_from_json_ld(html: &str) -> Option<LinkedInCommentContext> {
    for raw in json_ld_script_bodies(html) {
        let Ok(v) = serde_json::from_str::<JsonValue>(&raw) else {
            continue;
        };
        if let Some(ctx) = context_from_social_posting(&v) {
            return Some(ctx);
        }
        if let Some(arr) = v.as_array() {
            for item in arr {
                if let Some(ctx) = context_from_social_posting(item) {
                    return Some(ctx);
                }
            }
        }
        if let Some(graph) = v.get("@graph").and_then(JsonValue::as_array) {
            for item in graph {
                if let Some(ctx) = context_from_social_posting(item) {
                    return Some(ctx);
                }
            }
        }
    }
    None
}

fn json_ld_script_bodies(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find("application/ld+json") {
        let abs = search_from + rel;
        let Some(gt) = html[abs..].find('>') else {
            break;
        };
        let start = abs + gt + 1;
        let Some(end_rel) = lower[start..].find("</script") else {
            break;
        };
        let end = start + end_rel;
        out.push(html[start..end].trim().to_string());
        search_from = end;
    }
    out
}

fn context_from_social_posting(v: &JsonValue) -> Option<LinkedInCommentContext> {
    let ty = v.get("@type").and_then(JsonValue::as_str)?;
    if ty != "SocialMediaPosting" {
        return None;
    }
    let parent = v
        .get("articleBody")
        .and_then(JsonValue::as_str)
        .or_else(|| v.get("headline").and_then(JsonValue::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let comments = v.get("comment")?;
    let list: Vec<&JsonValue> = comments
        .as_array()
        .map_or_else(|| vec![comments], |arr| arr.iter().collect());
    for c in list {
        let Some(body) = c
            .get("text")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|s| s.len() >= 8)
        else {
            continue;
        };
        let author = c
            .get("author")
            .and_then(|a| {
                a.get("name")
                    .and_then(JsonValue::as_str)
                    .or_else(|| a.as_str())
            })
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("commenter");
        return Some(LinkedInCommentContext {
            parent_post: parent.to_string(),
            comment_author: author.to_string(),
            comment_body: collapse_ws(body),
        });
    }
    None
}

/// Insert newlines before guest chrome phrases so line-based extract still works on flat text.
fn rebreak_guest_text(flat: &str) -> String {
    let mut s = flat.to_string();
    for phrase in [
        " Report this comment",
        " Report this post",
        " Like ",
        " Reply",
        " Comment",
        " Comments",
    ] {
        if let Some(rest) = phrase.strip_prefix(' ') {
            s = s.replace(phrase, &format!("\n{rest}\n"));
        }
    }
    s
}

/// Pull full parent post + target comment from plain / lined page text.
#[must_use]
pub fn extract_comment_context(page_text: &str) -> Option<LinkedInCommentContext> {
    let text = page_text.trim();
    if text.is_empty() {
        return None;
    }
    let parent_post = extract_parent_post(text);
    let (author, body) = extract_comment_author_body(text)?;
    if body.trim().len() < 8 {
        return None;
    }
    Some(LinkedInCommentContext {
        parent_post,
        comment_author: author,
        comment_body: collapse_ws(&body),
    })
}

/// Full parent post text before the comments block (no char cap).
fn extract_parent_post(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut cut = lines.len();
    for (i, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("report this comment") {
            cut = i;
            break;
        }
        // "1 Comment" / "12 Comments" / bare "Comment"
        if is_comments_header(line) {
            cut = i;
            break;
        }
    }
    let mut kept: Vec<&str> = Vec::new();
    for line in &lines[..cut] {
        let lower = line.to_ascii_lowercase();
        if lower.contains("sign in")
            || lower.contains("agree & join")
            || lower.contains("agree and join")
            || lower.starts_with("report this")
            || looks_like_relative_time(line)
        {
            continue;
        }
        kept.push(line);
    }
    if kept.is_empty() {
        return collapse_ws(text);
    }
    kept.join("\n")
}

fn is_comments_header(line: &str) -> bool {
    let t = line.trim();
    let lower = t.to_ascii_lowercase();
    if lower == "comment" || lower == "comments" {
        return true;
    }
    // "1 Comment" / "12 Comments"
    let mut parts = t.split_whitespace();
    let Some(n) = parts.next() else {
        return false;
    };
    if !n.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let Some(word) = parts.next() else {
        return false;
    };
    parts.next().is_none() && matches!(word.to_ascii_lowercase().as_str(), "comment" | "comments")
}

fn extract_comment_author_body(text: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // Guest HTML: author, optional relative time, "Report this comment", then body.
    for i in 0..lines.len().saturating_sub(1) {
        let report_at = if lines[i]
            .to_ascii_lowercase()
            .starts_with("report this comment")
        {
            i
        } else {
            continue;
        };
        // Walk back past time / chrome to the author name.
        let mut author_idx = None;
        for j in (0..report_at).rev() {
            let cand = lines[j];
            let cl = cand.to_ascii_lowercase();
            if looks_like_relative_time(cand)
                || cl.contains("reaction")
                || cl == "like"
                || cl == "reply"
                || (cl.contains("comment") && cand.chars().count() < 24)
            {
                continue;
            }
            if cand.chars().count() < 80 {
                author_idx = Some(j);
                break;
            }
        }
        let Some(ai) = author_idx else {
            continue;
        };
        let author = lines[ai].to_string();
        for body in lines.iter().skip(report_at + 1) {
            let bl = body.to_ascii_lowercase();
            if bl == "like"
                || bl == "reply"
                || bl.contains("reaction")
                || looks_like_relative_time(body)
            {
                continue;
            }
            if body.chars().count() >= 12 {
                return Some((author, (*body).to_string()));
            }
        }
    }
    // Fallback: look for a known-ish comment paragraph after "Comment"
    let mut after_comment = false;
    let mut author = String::new();
    for line in &lines {
        let lower = line.to_ascii_lowercase();
        if lower.contains("comment") && line.chars().count() < 24 {
            after_comment = true;
            continue;
        }
        if !after_comment {
            continue;
        }
        if author.is_empty()
            && line.chars().count() < 80
            && !lower.contains("report")
            && !looks_like_relative_time(line)
        {
            author = (*line).to_string();
            continue;
        }
        if !author.is_empty()
            && line.chars().count() >= 12
            && !lower.starts_with("report")
            && !looks_like_relative_time(line)
            && lower != "like"
            && lower != "reply"
        {
            return Some((author, (*line).to_string()));
        }
    }
    None
}

fn looks_like_relative_time(s: &str) -> bool {
    let t = s.trim().to_ascii_lowercase();
    if t.is_empty() || t.len() > 12 {
        return false;
    }
    // 48m, 1h, 2d, 52m
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    digits > 0
        && t.chars()
            .nth(digits)
            .is_some_and(|c| matches!(c, 'm' | 'h' | 'd' | 'w'))
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Fetch the comment URL via Tor (same path as `/enrich`), draft a short reply for Slack.
///
/// # Errors
///
/// Returns an operator-facing message on bad URL, Tor miss, extract miss, or LLM failure.
pub async fn draft_comment_reply_for_slack(
    llm: &Arc<FailoverRouter>,
    url: &str,
) -> Result<String, String> {
    let (_target, ctx, reply) = draft_comment_reply_parts(llm, url).await?;
    Ok(format_slack_draft(&ctx, &reply))
}

/// Draft + ship a threaded reply via local `LinkedIn` MCP (`reply_to_comment`).
///
/// # Errors
///
/// Returns an operator-facing message on bad URL, missing comment id, draft failure, or MCP ship.
pub async fn ship_comment_reply_via_mcp(
    llm: &Arc<FailoverRouter>,
    url: &str,
) -> Result<String, String> {
    let (target, ctx, reply) = draft_comment_reply_parts(llm, url).await?;
    let comment_id = target.comment_id.as_deref().ok_or_else(|| {
        "URL must include dashCommentUrn (threaded reply needs a parent comment id)".to_string()
    })?;
    let post_urn = crate::publish::activity_post_urn(&target.activity_id);
    let parent_urn = crate::publish::parent_comment_urn(&target.activity_id, comment_id);
    let client = crate::publish::LinkedInMcpClient::new();
    let mcp_detail = client
        .reply_to_comment(&post_urn, &parent_urn, &reply)
        .await
        .map_err(|e| format!("LinkedIn MCP reply_to_comment: {e}"))?;
    Ok(format!(
        "Comment reply shipped via LinkedIn MCP\n\n\
Post:\n{post}\n\n\
Comment ({author}): {comment}\n\n\
Reply:\n{reply}\n\n\
MCP: {mcp}\n\
post_urn={post_urn}\n\
parent_comment_urn={parent_urn}",
        post = ctx.parent_post,
        author = ctx.comment_author,
        comment = ctx.comment_body,
        reply = reply.trim(),
        mcp = mcp_detail.trim(),
        post_urn = post_urn,
        parent_urn = parent_urn,
    ))
}

async fn draft_comment_reply_parts(
    llm: &Arc<FailoverRouter>,
    url: &str,
) -> Result<(LinkedInCommentTarget, LinkedInCommentContext, String), String> {
    let target = parse_linkedin_comment_url(url)?;
    info!(
        activity = %target.activity_id,
        comment = ?target.comment_id,
        "linkedin_comment: fetch start"
    );
    let socks = std::env::var("ITCY_TOR_SOCKS").unwrap_or_else(|_| DEFAULT_TOR_SOCKS.to_string());
    let fetcher = TorSocksFetcher::new(&socks).map_err(|e| format!("Tor client: {e}"))?;
    let html = fetcher
        .fetch_html(&target.url)
        .await
        .map_err(|e| format!("Tor fetch failed: {e}"))?;
    let Some(ctx) = extract_comment_context_from_html(&html) else {
        return Err(
            "could not isolate the comment on that page (login wall or layout). \
Paste the comment text in chat if you still want a draft."
                .into(),
        );
    };
    let reply = generate_reply(llm, &ctx).await?;
    Ok((target, ctx, reply))
}

async fn generate_reply(
    llm: &Arc<FailoverRouter>,
    ctx: &LinkedInCommentContext,
) -> Result<String, String> {
    let user = comment_reply_user_message(&ctx.parent_post, &ctx.comment_author, &ctx.comment_body);
    let messages = [
        LlmMessage::system(COMMENT_REPLY_SYSTEM_CORE),
        LlmMessage::user(user),
    ];
    let (resp, _trace) = llm
        .complete(TaskKind::Freeform, &messages)
        .await
        .map_err(|e| format!("LLM failed: {e}"))?;
    let raw = resp.message.content.trim();
    if raw.is_empty() {
        return Err("LLM returned an empty reply".into());
    }
    Ok(ensure_one_emoji(&sanitize_itcy_text(raw)))
}

/// Keep exactly one emoji glyph (inject owl if none; drop extras after the first).
#[must_use]
pub fn ensure_one_emoji(text: &str) -> String {
    let expanded = expand_emoji_shortcodes(text.trim());
    let n = count_emoji(&expanded);
    if n == 0 {
        let t = expanded.trim_end();
        if t.is_empty() {
            return "🦉".into();
        }
        return format!("{t} 🦉");
    }
    if n == 1 {
        return expanded;
    }
    // Keep text but remove emoji-like codepoints after the first.
    let mut out = String::new();
    let mut seen = false;
    for c in expanded.chars() {
        let is_emoji = count_emoji(&c.to_string()) > 0;
        if is_emoji {
            if seen {
                continue;
            }
            seen = true;
        }
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_slack_draft(ctx: &LinkedInCommentContext, reply: &str) -> String {
    format!(
        "Comment reply draft (paste on LinkedIn; not shipped)\n\n\
Post:\n{post}\n\n\
Comment ({author}): {comment}\n\n\
Reply:\n{reply}",
        post = ctx.parent_post,
        author = ctx.comment_author,
        comment = ctx.comment_body,
        reply = reply.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_URL: &str = "https://www.linkedin.com/feed/update/urn:li:activity:7496571584363741184/?dashCommentUrn=urn%3Ali%3Afsd_comment%3A%287496664683873992704%2Curn%3Ali%3Aactivity%3A7496571584363741184%29";

    #[test]
    fn parse_activity_and_comment_from_dash_urn() {
        let t = parse_linkedin_comment_url(SAMPLE_URL).expect("parse");
        assert_eq!(t.activity_id, "7496571584363741184");
        assert_eq!(t.comment_id.as_deref(), Some("7496664683873992704"));
    }

    #[test]
    fn parse_activity_only() {
        let t = parse_linkedin_comment_url(
            "https://www.linkedin.com/feed/update/urn:li:activity:1234567890/",
        )
        .expect("parse");
        assert_eq!(t.activity_id, "1234567890");
        assert!(t.comment_id.is_none());
    }

    #[test]
    fn parse_rejects_non_linkedin() {
        assert!(parse_linkedin_comment_url("https://example.com/x").is_err());
    }

    #[test]
    fn extract_from_guest_json_ld() {
        let html = r#"<html><script type="application/ld+json">
{"@context":"http://schema.org","@type":"SocialMediaPosting","headline":"OxiSH short","articleBody":"OxiSH is a memory-safe SSH server, and it's been in the works for 20 months. Dirkjan Ochtman built it in Rust.","comment":[{"@type":"Comment","text":"20 months for an SSH server sounds about right for the diff-by-diff pace memory safety actually demands, not the sprint this writeup is pretending it was.","author":{"@type":"Person","name":"Valentyn Kit"}}],"commentCount":1}
</script></html>"#;
        let ctx = extract_comment_context_from_html(html).expect("json-ld");
        assert!(ctx.comment_author.contains("Valentyn"));
        assert!(ctx.comment_body.contains("diff-by-diff"));
        assert!(ctx.parent_post.contains("OxiSH"));
        assert!(ctx.parent_post.contains("Dirkjan"));
    }

    #[test]
    fn extract_valentyn_style_guest_page() {
        let page = r"
Interchouette - ITC's Post
OxiSH is a memory-safe SSH server, and it's been in the works for 20 months. Dirkjan Ochtman built it in Rust.

1 Comment
Valentyn Kit
48m
Report this comment
20 months for an SSH server sounds about right for the diff-by-diff pace memory safety actually demands, not the sprint this writeup is pretending it was.
Like
Reply
";
        let ctx = extract_comment_context(page).expect("extract");
        assert!(ctx.comment_author.contains("Valentyn"));
        assert!(ctx.comment_body.contains("20 months"));
        assert!(ctx.parent_post.contains("OxiSH"));
        assert!(ctx.parent_post.contains("Dirkjan"));
        assert!(!ctx.parent_post.contains("Valentyn"));
        assert!(!ctx.parent_post.contains("Report this"));
    }

    #[test]
    fn ensure_one_emoji_injects_and_strips() {
        assert!(ensure_one_emoji("plain text").contains('🦉'));
        assert_eq!(count_emoji(&ensure_one_emoji("hi 🦉 there 🦀")), 1);
        assert_eq!(count_emoji(&ensure_one_emoji("already 🦉")), 1);
    }

    #[test]
    fn parent_comment_urn_from_parsed_ids() {
        let t = parse_linkedin_comment_url(SAMPLE_URL).expect("parse");
        let cid = t.comment_id.as_deref().expect("comment");
        assert_eq!(
            crate::publish::parent_comment_urn(&t.activity_id, cid),
            "urn:li:comment:(urn:li:activity:7496571584363741184,7496664683873992704)"
        );
    }
}

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! X/Twitter publisher after tweet BAT (playground soft ship; production via Brave).
//!
//! Paid X API user-context is available behind `ITCY_X_SHIP_VIA=api`.

use crate::publish::{
    PublishAuditStore, PublishAuditWrite, PublishError, PublishMode, PublishRequest, PublishResult,
};
use crate::sources::twitter::{load_twitter_creds, TwitterCreds};
use crate::sources::url_hygiene::{x_status_public_url, TWITTER_API_V2_BASE, X_PUBLIC_HANDLE};
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tracing::{info, warn};

/// Request for one X post (text + optional quote tweet).
#[derive(Debug, Clone)]
pub struct XPublishRequest {
    pub tweet_id: Option<String>,
    pub pubs_pr_number: Option<u64>,
    pub body: String,
    pub quote_tweet_id: Option<String>,
}

/// Resolves X ship mode: `ITCY_X_PUBLISH_MODE` → `[x].publish_mode` → `fallback`.
///
/// # Errors
///
/// Returns [`PublishError::Config`] when the mode string is unknown.
pub fn resolve_x_publish_mode(fallback: &str) -> Result<PublishMode, PublishError> {
    if let Ok(raw) = std::env::var("ITCY_X_PUBLISH_MODE") {
        let raw = raw.trim();
        if !raw.is_empty() {
            return PublishMode::parse(raw);
        }
    }
    if let Some(from_disk) = crate::publish::read_section_publish_mode_from_config_toml("x") {
        return PublishMode::parse(&from_disk);
    }
    PublishMode::parse(fallback)
}

/// Ships a tweet after BAT. Playground by default; production uses Brave (API via env later).
///
/// # Errors
///
/// Returns [`PublishError`] when mode, credentials, Brave, or the X HTTP call fail.
pub async fn ship_x_post(
    state_db_path: impl AsRef<Path>,
    mode_fallback: &str,
    mut request: XPublishRequest,
    mode_override: Option<PublishMode>,
) -> Result<PublishResult, PublishError> {
    let mode = match mode_override {
        Some(m) => m,
        None => resolve_x_publish_mode(mode_fallback)?,
    };
    prepare_x_publish_request(&mut request);
    info!(
        mode = mode.as_str(),
        tweet_id = request.tweet_id.as_deref().unwrap_or(""),
        quote = request.quote_tweet_id.as_deref().unwrap_or(""),
        "publish: x ship starting"
    );
    let audit = PublishAuditStore::open(state_db_path.as_ref())
        .map_err(|e| PublishError::Other(e.to_string()))?;
    let linkedin_req = PublishRequest {
        draft_id: request.tweet_id.clone(),
        pubs_pr_number: request.pubs_pr_number,
        body: request.body.clone(),
    };
    match ship_x_inner(mode, &request).await {
        Ok(result) => {
            if let Err(e) = audit.insert(&PublishAuditWrite::from_ok(&linkedin_req, &result)) {
                warn!(error = %e, "publish: x audit insert failed after ok ship");
            }
            info!(detail = %result.detail, "publish: x ship ok");
            Ok(result)
        }
        Err(err) => {
            if let Err(e) = audit.insert(&PublishAuditWrite::from_err(&linkedin_req, mode, &err)) {
                warn!(error = %e, "publish: x audit insert failed after ship error");
            }
            Err(err)
        }
    }
}

async fn ship_x_inner(
    mode: PublishMode,
    request: &XPublishRequest,
) -> Result<PublishResult, PublishError> {
    match mode {
        PublishMode::Playground => Ok(playground_x_post(request)),
        PublishMode::Production => production_x_post(request).await,
    }
}

fn playground_x_post(request: &XPublishRequest) -> PublishResult {
    let slug = request
        .tweet_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .map_or_else(|| "unknown".into(), |s| s.replace(['/', ' '], "-"));
    let status_id = format!("playground-{slug}");
    let url = x_status_public_url(&status_id);
    let texts = tweet_texts_for_api(&request.body);
    let reply = texts.get(1).map(|_| format!("{url}-2"));
    PublishResult {
        mode: PublishMode::Playground,
        linkedin_urn: Some(status_id),
        linkedin_url: Some(url.clone()),
        detail: x_ship_detail(
            Some(&url),
            request.quote_tweet_id.as_deref(),
            reply.as_deref(),
        ),
    }
}

async fn production_x_post(request: &XPublishRequest) -> Result<PublishResult, PublishError> {
    let via = std::env::var("ITCY_X_SHIP_VIA")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if via == "api" {
        return api_x_post(request).await;
    }
    brave_x_post(request).await
}

async fn brave_x_post(request: &XPublishRequest) -> Result<PublishResult, PublishError> {
    let (text, reply) = ship_texts(&request.body)?;
    let script = resolve_post_twitter_cmd().ok_or_else(|| {
        PublishError::Other("scripts/post-twitter.sh not found (Brave X ship)".into())
    })?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp_path = std::env::temp_dir().join(format!("itcy-x-post-{stamp}.txt"));
    let reply_path = reply
        .as_ref()
        .map(|_| std::env::temp_dir().join(format!("itcy-x-reply-{stamp}.txt")));
    std::fs::write(&tmp_path, text.as_bytes())
        .map_err(|e| PublishError::Other(format!("write tweet file: {e}")))?;
    if let (Some(path), Some(body)) = (reply_path.as_ref(), reply.as_ref()) {
        std::fs::write(path, body.as_bytes())
            .map_err(|e| PublishError::Other(format!("write reply file: {e}")))?;
    }
    let mut cmd = Command::new(&script);
    cmd.arg(&tmp_path);
    if let Some(qid) = request
        .quote_tweet_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        cmd.arg(qid);
    }
    if let Some(path) = reply_path.as_ref() {
        cmd.env("ITCY_TWITTER_REPLY_TEXT_FILE", path);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    info!(script = %script.display(), "publish: x Brave ship starting");
    let out = cmd
        .output()
        .await
        .map_err(|e| PublishError::Other(format!("Brave post spawn: {e}")))?;
    let _ = std::fs::remove_file(&tmp_path);
    if let Some(path) = reply_path.as_ref() {
        let _ = std::fs::remove_file(path);
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        return Err(PublishError::Other(format!(
            "Brave X post failed (exit {:?}): {stderr} {stdout}",
            out.status.code()
        )));
    }
    let parsed: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| {
        PublishError::Other(format!(
            "Brave X post JSON parse: {e}; stdout={stdout}; stderr={stderr}"
        ))
    })?;
    if parsed.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let detail = parsed
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or(stdout.as_str());
        return Err(PublishError::Other(format!(
            "Brave X post refused: {detail}"
        )));
    }
    let quote_id = request
        .quote_tweet_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let posted = posted_status_from_brave(&parsed, quote_id);
    if posted.is_none() {
        warn!(
            quote = quote_id.unwrap_or(""),
            stdout = %stdout,
            "publish: Brave JSON did not include our posted tweet URL"
        );
    }
    let reply_url = parsed
        .get("reply_url")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (id, public) = posted.unzip();
    Ok(PublishResult {
        mode: PublishMode::Production,
        linkedin_urn: id,
        linkedin_url: public.clone(),
        detail: x_ship_detail(public.as_deref(), quote_id, reply_url),
    })
}

/// Keep `quote_tweet_id` when set. A bare X status URL in the body is the
/// operator Link choice - do not clear the quote and do not strip that URL.
const fn prepare_x_publish_request(_request: &mut XPublishRequest) {
    // Intentionally empty: previous code cleared quote whenever an X status URL
    // remained in the body. That was wrong (URL ≠ Quote Tweet on X).
}

fn ship_texts(body: &str) -> Result<(String, Option<String>), PublishError> {
    let mut iter = tweet_texts_for_api(body)
        .into_iter()
        .filter(|s| !s.trim().is_empty());
    let first = iter
        .next()
        .ok_or_else(|| PublishError::Other("empty tweet text after footer strip".into()))?;
    if !crate::sources::tweet_thread::fits_x_limit(&first) {
        return Err(PublishError::Other(format!(
            "refusing to ship: root tweet is {} weighted chars (X limit {})",
            crate::sources::tweet_thread::x_weighted_len(&first),
            crate::sources::tweet_thread::X_CHAR_LIMIT
        )));
    }
    let reply = iter.next();
    if let Some(ref r) = reply {
        if !crate::sources::tweet_thread::fits_x_limit(r) {
            return Err(PublishError::Other(format!(
                "refusing to ship: reply tweet is {} weighted chars (X limit {})",
                crate::sources::tweet_thread::x_weighted_len(r),
                crate::sources::tweet_thread::X_CHAR_LIMIT
            )));
        }
    }
    let own_at = format!("@{X_PUBLIC_HANDLE}");
    let own_at_lower = own_at.to_ascii_lowercase();
    if first
        .lines()
        .next()
        .is_some_and(|l| l.trim().eq_ignore_ascii_case(&own_at))
        || first.to_ascii_lowercase().contains(&own_at_lower)
    {
        return Err(PublishError::Other(format!(
            "refusing to ship: tweet text still contains {own_at} (own-handle spam)"
        )));
    }
    Ok((first, reply))
}

fn resolve_post_twitter_cmd() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("ITCY_TWITTER_POST_CMD") {
        let p = PathBuf::from(raw.trim());
        if p.is_file() {
            return Some(p);
        }
    }
    [
        PathBuf::from("scripts/post-twitter.sh"),
        PathBuf::from("../scripts/post-twitter.sh"),
        crate::paths::product_join("scripts/post-twitter.sh"),
    ]
    .into_iter()
    .find(|c| c.is_file())
}

async fn api_x_post(request: &XPublishRequest) -> Result<PublishResult, PublishError> {
    let creds = load_twitter_creds().map_err(|e| PublishError::Credentials(e.to_string()))?;
    if creds.has_bearer() && !creds.has_user_context() {
        return Err(PublishError::Credentials(
            "app-only TWITTER_BEARER cannot tweet; set TWITTER_API_KEY / TWITTER_API_SECRET / TWITTER_ACCESS_TOKEN / TWITTER_ACCESS_TOKEN_SECRET in .twitter (or omit ITCY_X_SHIP_VIA=api to use Brave)"
                .into(),
        ));
    }
    if !creds.has_user_context() {
        return Err(PublishError::Credentials(
            "X API ship needs user-context secrets in .twitter (or omit ITCY_X_SHIP_VIA=api to use Brave)"
                .into(),
        ));
    }
    let (text, reply) = ship_texts(&request.body)?;
    let mut payload = serde_json::json!({ "text": text });
    if let Some(qid) = request
        .quote_tweet_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        payload["quote_tweet_id"] = serde_json::Value::String(qid.to_string());
    }
    let url = format!("{TWITTER_API_V2_BASE}/tweets");
    let auth = oauth1_header("POST", &url, &creds)?;
    let http = reqwest::Client::new();
    let resp = http
        .post(&url)
        .header("Authorization", auth)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| PublishError::Http(e.to_string()))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(PublishError::Http(format!("X tweets API {status}: {body}")));
    }
    let id = parse_created_tweet_id(&body).unwrap_or_else(|| "unknown".into());
    let public = x_status_public_url(&id);
    let reply_url = if let Some(reply_text) = reply {
        api_reply_tweet(&http, &creds, &id, &reply_text)
            .await
            .ok()
            .map(|rid| x_status_public_url(&rid))
    } else {
        None
    };
    Ok(PublishResult {
        mode: PublishMode::Production,
        linkedin_urn: Some(id),
        linkedin_url: Some(public.clone()),
        detail: x_ship_detail(
            Some(&public),
            request.quote_tweet_id.as_deref(),
            reply_url.as_deref(),
        ),
    })
}

async fn api_reply_tweet(
    http: &reqwest::Client,
    creds: &TwitterCreds,
    parent_id: &str,
    text: &str,
) -> Result<String, PublishError> {
    let payload = serde_json::json!({
        "text": text,
        "reply": { "in_reply_to_tweet_id": parent_id }
    });
    let url = format!("{TWITTER_API_V2_BASE}/tweets");
    let auth = oauth1_header("POST", &url, creds)?;
    let resp = http
        .post(&url)
        .header("Authorization", auth)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| PublishError::Http(e.to_string()))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(PublishError::Http(format!("X reply API {status}: {body}")));
    }
    Ok(parse_created_tweet_id(&body).unwrap_or_else(|| "unknown".into()))
}

/// Drop Slack footer / ID header; keep tweet text (+ publisher / X status URL).
/// Operator Link:1 stays in the body. Quote is a separate field (`quote_tweet_id`).
#[must_use]
pub fn tweet_text_for_api(body: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if is_tweet_operator_chrome(t) {
            if is_tweet_footer_break(t) {
                break;
            }
            continue;
        }
        lines.push(line);
    }
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let joined = lines.join("\n").trim().to_string();
    let joined = crate::sources::tweet_footer::strip_own_x_handle(&joined);
    crate::llm::sanitize::expand_emoji_shortcodes(&joined)
}

fn is_tweet_operator_chrome(t: &str) -> bool {
    t.starts_with("Tweet ID:")
        || t.starts_with("XPOST ID:")
        || t.starts_with("Cite")
        || t.starts_with("Link:")
        || t.starts_with("X:")
        || t.starts_with("Quote")
        || t.starts_with("Swap:")
        || t.starts_with("Numbered URLs after")
        || t.starts_with("0 = no cite")
        || t.starts_with("0 = no link")
        || t.starts_with("Written by AI")
        || t.starts_with("Sources")
        || crate::sources::tweet_thread::is_thread_chrome_line(t)
        || (t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains("http"))
}

fn is_tweet_footer_break(t: &str) -> bool {
    t.starts_with("Cite")
        || t.starts_with("Link:")
        || t.starts_with("X:")
        || t.starts_with("Quote")
        || t.starts_with("Swap:")
        || t.starts_with("Numbered URLs after")
        || t.starts_with("0 = no cite")
        || t.starts_with("0 = no link")
        || t.starts_with("Written by AI")
        || t.starts_with("Sources")
}

/// Root tweet first; optional reply second (tags + publisher URL when over 280).
#[must_use]
pub fn tweet_texts_for_api(body: &str) -> Vec<String> {
    crate::sources::tweet_thread::layout_x_thread(&tweet_text_for_api(body))
}

fn posted_status_from_brave(
    parsed: &serde_json::Value,
    quote_id: Option<&str>,
) -> Option<(String, String)> {
    let id = parsed
        .get("status_id")
        .and_then(serde_json::Value::as_str)?;
    let id = id.trim();
    if id.is_empty() || id == "unknown" {
        return None;
    }
    let public = parsed
        .get("url")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(|| x_status_public_url(id), ToOwned::to_owned);
    if is_quote_source_url(id, &public, quote_id) {
        return None;
    }
    Some((id.to_string(), public))
}

fn is_quote_source_url(id: &str, url: &str, quote_id: Option<&str>) -> bool {
    let Some(qid) = quote_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    id == qid || crate::sources::url_hygiene::x_status_id(url).as_deref() == Some(qid)
}

fn x_ship_detail(
    posted_url: Option<&str>,
    quote_id: Option<&str>,
    reply_url: Option<&str>,
) -> String {
    let posted = posted_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Posted (tweet URL unresolved)");
    let mut lines = vec![posted.to_string()];
    if let Some(reply) = reply_url.map(str::trim).filter(|s| !s.is_empty()) {
        lines.push(format!("Reply {reply}"));
    }
    if let Some(qid) = quote_id.map(str::trim).filter(|s| !s.is_empty()) {
        lines.push(format!("Quote of https://x.com/i/status/{qid}"));
    }
    lines.join("\n")
}

fn parse_created_tweet_id(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("data")?.get("id")?.as_str().map(ToString::to_string)
}

fn oauth1_header(method: &str, url: &str, creds: &TwitterCreds) -> Result<String, PublishError> {
    let ck = creds.api_key.as_deref().unwrap_or("");
    let cs = creds.api_secret.as_deref().unwrap_or("");
    let token = creds.access_token.as_deref().unwrap_or("");
    let ts = creds.access_token_secret.as_deref().unwrap_or("");
    let nonce = format!("{}{}", unix_secs(), std::process::id());
    let timestamp = unix_secs().to_string();
    let mut params = vec![
        ("oauth_consumer_key", ck.to_string()),
        ("oauth_nonce", nonce),
        ("oauth_signature_method", "HMAC-SHA1".into()),
        ("oauth_timestamp", timestamp),
        ("oauth_token", token.to_string()),
        ("oauth_version", "1.0".into()),
    ];
    params.sort_by(|a, b| a.0.cmp(b.0));
    let param_str = params
        .iter()
        .map(|(k, v)| format!("{}={}", pct(k), pct(v)))
        .collect::<Vec<_>>()
        .join("&");
    let base = format!("{}&{}&{}", pct(method), pct(url), pct(&param_str));
    let signing_key = format!("{}&{}", pct(cs), pct(ts));
    let mut mac = Hmac::<Sha1>::new_from_slice(signing_key.as_bytes())
        .map_err(|e| PublishError::Other(format!("hmac: {e}")))?;
    mac.update(base.as_bytes());
    let sig = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    params.push(("oauth_signature", sig));
    let mut header = String::from("OAuth ");
    for (i, (k, v)) in params.iter().enumerate() {
        if i > 0 {
            header.push_str(", ");
        }
        let _ = write!(header, "{k}=\"{}\"", pct(v));
    }
    Ok(header)
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn pct(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if matches!(
            b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~'
        ) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ship_text_has_x_status(body: &str) -> bool {
        tweet_text_for_api(body)
            .lines()
            .any(|l| crate::sources::url_hygiene::is_x_status_url(l.trim()))
    }

    #[test]
    fn playground_records_quote_vs_link() {
        let link = playground_x_post(&XPublishRequest {
            tweet_id: Some("TWEET-20260813-000001".into()),
            pubs_pr_number: Some(1),
            body: "hi".into(),
            quote_tweet_id: None,
        });
        assert!(link.linkedin_url.unwrap().contains("Interchouette"));
        assert!(link.detail.contains("https://x.com/Interchouette/status/"));
        assert!(!link.detail.contains("Quote of"));
        let quote = playground_x_post(&XPublishRequest {
            tweet_id: Some("TWEET-20260813-000001".into()),
            pubs_pr_number: Some(1),
            body: "hi".into(),
            quote_tweet_id: Some("99".into()),
        });
        assert!(quote.linkedin_url.unwrap().contains("Interchouette"));
        assert!(quote.detail.contains("https://x.com/Interchouette/status/"));
        assert!(quote.detail.contains("Quote of https://x.com/i/status/99"));
        assert!(!quote.detail.contains("x.com/someone"));
    }

    #[test]
    fn brave_json_ignores_quote_source_as_posted() {
        let v = serde_json::json!({
            "ok": true,
            "status_id": "111",
            "url": "https://x.com/someone/status/111"
        });
        assert!(posted_status_from_brave(&v, Some("111")).is_none());
        let own = serde_json::json!({
            "ok": true,
            "status_id": "222",
            "url": "https://x.com/Interchouette/status/222"
        });
        let (id, url) = posted_status_from_brave(&own, Some("111")).expect("own status");
        assert_eq!(id, "222");
        assert!(url.contains("Interchouette/status/222"));
        assert!(x_ship_detail(None, Some("111"), None).contains("URL unresolved"));
        assert!(x_ship_detail(Some(&url), Some("111"), None)
            .contains("Quote of https://x.com/i/status/111"));
        assert!(x_ship_detail(
            Some(&url),
            None,
            Some("https://x.com/Interchouette/status/333")
        )
        .contains("Reply https://x.com/Interchouette/status/333"));
    }

    #[test]
    fn playground_splits_overlong_publisher_body() {
        let body = "\
🦉 GitHub Models' retirement feels like a quiet end to a promising experiment.
Sad to see such a tool fade-especially when free alternatives are scarce.

Microsoft Foundry and Copilot?
Not exactly the open-source dream we hoped for.

Builders, keep an eye on migration paths.
The future of AI tools is still in flux.

#AI #GitHub #ModelRetirement

https://blog.dante.company/en/articles/github-models-retirement-migration-2026-07-02";
        let r = playground_x_post(&XPublishRequest {
            tweet_id: Some("XPOST-20260814-000010".into()),
            pubs_pr_number: Some(18),
            body: body.into(),
            quote_tweet_id: None,
        });
        assert!(r.detail.contains("-2"), "{}", r.detail);
        let texts = tweet_texts_for_api(body);
        assert_eq!(texts.len(), 2);
        assert!(
            texts[0].contains("GitHub Models"),
            "root is the start: {}",
            texts[0]
        );
        assert!(
            texts[1].contains("https://blog.dante.company"),
            "reply carries the link: {}",
            texts[1]
        );
    }

    #[test]
    fn tinyboot_style_body_keeps_x_url_when_no_quote() {
        let body = "\
Tweet ID: TWEET-1

@Interchouette
⚡ tinyboot is a clever Rust bootloader that fits in just 1920 bytes-perfect for microcontrollers with tight memory.

It leaves every byte of user flash free, which means more room for your app.

That’s the kind of smart engineering that makes embedded systems easier to build.

#Rust #Embedded #OpenSource

https://x.com/AstraKernel/status/2088224406187413962

Link: 1
0 = no link. /change_url TWEET-1 <0|1|2|3|url>
1. https://x.com/AstraKernel/status/2088224406187413962";
        let texts = tweet_texts_for_api(body);
        assert_eq!(texts.len(), 2, "tags+URL push a reply: {texts:?}");
        assert!(texts[0].contains("tinyboot"));
        assert!(!texts
            .join("\n")
            .to_ascii_lowercase()
            .contains("@interchouette"));
        assert!(
            texts[1].contains("x.com/AstraKernel"),
            "reply carries the X URL: {}",
            texts[1]
        );
        assert!(
            texts[1].contains("#Rust") && texts[1].contains("#Embedded"),
            "tags on the reply: {}",
            texts[1]
        );
        assert!(
            !texts[0].contains('#'),
            "root stays commentary: {}",
            texts[0]
        );
        assert!(
            !texts[0].trim_end().ends_with("easier"),
            "must not mid-cut: {}",
            texts[0]
        );
        assert!(ship_text_has_x_status(body));
    }

    #[test]
    fn quote_ship_keeps_quote_and_operator_link_url() {
        // Regression: XPOST-20260820-000046 — quote + Link:1 = X status.
        // Must keep quote AND keep the operator-chosen URL (do not strip / do not clear quote).
        let body = "\
Tweet ID: TWEET-20260820-000046

📜 @github’s 2026 outage crisis is real, 257 incidents, 48 major outages, and a 50% repo download error rate. 🚀 The root? Autoscaling fails + VS Code retry storms. 🦀 But they’re not just fixing it, they’re shipping fixes and new features like stacked PRs. #CloudOps #DevTools #GitHub #OutageFixes

https://x.com/acolombiadev/status/2089811385899160055

Link: 1
0 = no link. /change_url TWEET-20260820-000046 <0|1|2|3|url>
1. https://x.com/acolombiadev/status/2089811385899160055
2. https://x.com/ashnichrist/status/2090551150214836367

Written by AI - ITCy - model ollama/qwen3:8b - tokens in:6146 out:123";
        let mut req = XPublishRequest {
            tweet_id: Some("XPOST-20260820-000046".into()),
            pubs_pr_number: Some(45),
            body: body.into(),
            quote_tweet_id: Some("2089811385899160055".into()),
        };
        prepare_x_publish_request(&mut req);
        assert_eq!(req.quote_tweet_id.as_deref(), Some("2089811385899160055"));
        assert!(
            ship_text_has_x_status(&req.body),
            "operator Link:1 X URL must stay in the body"
        );
        let (text, reply) = ship_texts(&req.body).expect("ship texts");
        let reply = reply.expect("URL+tags force a reply so root fits");
        assert!(crate::sources::tweet_thread::fits_x_limit(&text), "{text}");
        assert!(
            crate::sources::tweet_thread::fits_x_limit(&reply),
            "{reply}"
        );
        assert!(text.contains("outage crisis"), "{text}");
        assert!(
            reply.contains("acolombiadev") || reply.contains("2089811385899160055"),
            "reply keeps the cite URL: {reply}"
        );
        assert!(
            reply.contains("#CloudOps") || text.contains("#CloudOps"),
            "tags still ship: root={text} reply={reply}"
        );
    }

    #[test]
    fn ship_texts_refuses_inline_own_handle() {
        let err = ship_texts("Builders ping @Interchouette for help").unwrap_err();
        assert!(err.to_string().contains("@Interchouette"), "{err}");
        let (text, reply) = ship_texts("@Interchouette\n\nHello builders.").expect("scrub");
        assert_eq!(text, "Hello builders.");
        assert!(reply.is_none());
    }

    #[test]
    fn tweet_text_strips_link_footer() {
        let text = tweet_text_for_api(
            "Tweet ID: TWEET-1\n\nHello builders\n\nhttps://labs.sogeti.com/a\n\nLink: 1\n0 = no link. /change_url TWEET-1 <0|1|2|3|url>\n1. https://labs.sogeti.com/a\n",
        );
        assert!(text.contains("Hello builders"));
        assert!(text.contains("https://labs.sogeti.com/a"));
        assert!(!text.contains("Tweet ID:"));
        assert!(!text.contains("Link:"));
        assert!(!text.contains("0 = no link"));
    }

    #[test]
    fn tweet_text_expands_emoji_shortcodes() {
        let text = tweet_text_for_api("Hello! :owl: :computer:\n");
        assert!(text.contains('🦉'));
        assert!(text.contains('💻'));
        assert!(!text.contains(':'));
    }

    #[test]
    fn tweet_text_expands_operator_intro_shortcodes() {
        let text = tweet_text_for_api(
            "Tweet ID: TWEET-1\n\nHello! :feet: I’m ITCy. Let’s build something fun. :owl::computer:\n\nCite = option 1\n",
        );
        assert!(text.contains('🦉'));
        assert!(text.contains('💻'));
        assert!(text.contains('👣'));
        assert!(!text.contains(":owl:"));
        assert!(!text.contains("Tweet ID:"));
    }
}

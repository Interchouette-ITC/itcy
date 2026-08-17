// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Twitter/X as a **discovery tool** (not corpus). Creds in repo-local `.twitter`.
//!
//! Prefer Bearer API when `TWITTER_BEARER` is set; otherwise headed Brave CDP via
//! `scripts/fetch-twitter-pulse.sh` (warm `pw/profile-x`, scrapes on a disposable copy).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use thiserror::Error;
use tokio::process::Command;
use tracing::{info, warn};

use crate::sources::url_hygiene::TWITTER_API_V2_BASE;

/// Loaded credentials (never log secret values).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitterCreds {
    pub user: Option<String>,
    pub password: Option<String>,
    pub bearer: Option<String>,
    pub api_key: Option<String>,
    pub api_secret: Option<String>,
    pub access_token: Option<String>,
    pub access_token_secret: Option<String>,
}

impl TwitterCreds {
    /// True when any usable secret is present.
    #[must_use]
    pub fn has_secret(&self) -> bool {
        self.bearer.as_ref().is_some_and(|s| !s.is_empty())
            || self.password.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// True when OAuth 1.0a user-context can create tweets (not app-only Bearer).
    #[must_use]
    pub fn has_user_context(&self) -> bool {
        self.api_key.as_ref().is_some_and(|s| !s.is_empty())
            && self.api_secret.as_ref().is_some_and(|s| !s.is_empty())
            && self.access_token.as_ref().is_some_and(|s| !s.is_empty())
            && self
                .access_token_secret
                .as_ref()
                .is_some_and(|s| !s.is_empty())
    }

    /// True when app-only Bearer can call X API.
    #[must_use]
    pub fn has_bearer(&self) -> bool {
        self.bearer.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// Short operator line (no secrets).
    #[must_use]
    pub fn status_line(&self) -> String {
        let user = self.user.as_deref().unwrap_or("-");
        let auth = if self.has_bearer() {
            "bearer"
        } else if resolve_twitter_pulse_cmd().is_some() {
            "pw-pulse"
        } else if self.password.as_ref().is_some_and(|s| !s.is_empty()) {
            "password"
        } else {
            "none"
        };
        format!("twitter creds: user={user} auth={auth}")
    }
}

/// One ephemeral hit for digest ranking (not written to `sources`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct TwitterHit {
    pub title: String,
    pub url: String,
    pub subject: String,
    pub detail: String,
    /// `following` (home timeline) or `twitter` (keyword search).
    #[serde(default = "default_hit_lane")]
    pub lane: String,
    /// Search query that produced this hit (empty for home timeline).
    #[serde(default)]
    pub query: String,
}

fn default_hit_lane() -> String {
    "twitter".into()
}

/// Tool errors (operator-facing).
#[derive(Debug, Error)]
pub enum TwitterToolError {
    #[error("twitter: {0}")]
    Io(#[from] std::io::Error),
    #[error("twitter: {0}")]
    Other(String),
}

/// Candidate `.twitter` paths (cwd-relative, then product root).
#[must_use]
fn twitter_cred_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(".twitter"));
        out.push(cwd.join("../.twitter"));
    }
    out.push(crate::paths::product_join(".twitter"));
    out
}

/// Loads `.twitter`: KEY=value (`TWITTER_USER` / `TWITTER_PASSWORD` / `TWITTER_BEARER`)
/// or legacy two-line `user\\nsecret`.
///
/// # Errors
///
/// Returns [`TwitterToolError`] when no file exists or content is empty.
pub fn load_twitter_creds() -> Result<TwitterCreds, TwitterToolError> {
    let path = twitter_cred_candidates()
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| TwitterToolError::Other(".twitter not found".into()))?;
    load_twitter_creds_from(&path)
}

/// Loads creds from an explicit path.
///
/// # Errors
///
/// Returns [`TwitterToolError`] on empty or unreadable content.
pub fn load_twitter_creds_from(path: &Path) -> Result<TwitterCreds, TwitterToolError> {
    let text = std::fs::read_to_string(path)?;
    let mut user = None;
    let mut password = None;
    let mut bearer = None;
    let mut api_key = None;
    let mut api_secret = None;
    let mut access_token = None;
    let mut access_token_secret = None;
    let mut plain = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("TWITTER_USER=") {
            user = Some(trim_val(rest));
        } else if let Some(rest) = line.strip_prefix("TWITTER_PASSWORD=") {
            password = Some(trim_val(rest));
        } else if let Some(rest) = line.strip_prefix("TWITTER_BEARER=") {
            bearer = Some(trim_val(rest));
        } else if let Some(rest) = line.strip_prefix("TWITTER_AUTH_TOKEN=") {
            password = Some(trim_val(rest));
        } else if let Some(rest) = line.strip_prefix("TWITTER_API_KEY=") {
            api_key = Some(trim_val(rest));
        } else if let Some(rest) = line.strip_prefix("TWITTER_API_SECRET=") {
            api_secret = Some(trim_val(rest));
        } else if let Some(rest) = line.strip_prefix("TWITTER_ACCESS_TOKEN=") {
            access_token = Some(trim_val(rest));
        } else if let Some(rest) = line.strip_prefix("TWITTER_ACCESS_TOKEN_SECRET=") {
            access_token_secret = Some(trim_val(rest));
        } else {
            plain.push(line.to_string());
        }
    }
    if user.is_none() && password.is_none() && bearer.is_none() {
        if plain.len() >= 2 {
            user = Some(plain[0].clone());
            password = Some(plain[1].clone());
        } else if plain.len() == 1 {
            bearer = Some(plain[0].clone());
        }
    }
    let creds = TwitterCreds {
        user,
        password,
        bearer,
        api_key,
        api_secret,
        access_token,
        access_token_secret,
    };
    if !creds.has_secret() && creds.user.is_none() {
        return Err(TwitterToolError::Other(format!(
            "{} is empty",
            path.display()
        )));
    }
    Ok(creds)
}

fn trim_val(raw: &str) -> String {
    raw.trim().trim_matches('"').to_string()
}

/// Resolves `scripts/fetch-twitter-pulse.sh` (env override or product path).
#[must_use]
pub fn resolve_twitter_pulse_cmd() -> Option<String> {
    if let Ok(raw) = std::env::var("ITCY_TWITTER_PULSE_CMD") {
        let t = raw.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    for candidate in [
        PathBuf::from("scripts/fetch-twitter-pulse.sh"),
        PathBuf::from("../scripts/fetch-twitter-pulse.sh"),
        crate::paths::product_join("scripts/fetch-twitter-pulse.sh"),
    ] {
        if candidate.is_file() {
            return Some(candidate.display().to_string());
        }
    }
    None
}

/// Live Twitter/X discovery (ephemeral). Bearer API when present; else PW pulse script.
pub struct TwitterTool {
    creds: TwitterCreds,
}

impl TwitterTool {
    /// Builds from loaded creds.
    #[must_use]
    pub const fn new(creds: TwitterCreds) -> Self {
        Self { creds }
    }

    /// Loads `.twitter` and builds the tool.
    ///
    /// # Errors
    ///
    /// Returns [`TwitterToolError`] when creds cannot be loaded.
    pub fn from_disk() -> Result<Self, TwitterToolError> {
        Ok(Self::new(load_twitter_creds()?))
    }

    /// Fetch one status by numeric id (Bearer). Caller falls back to `browse_url` if this fails.
    ///
    /// # Errors
    ///
    /// Returns [`TwitterToolError`] on HTTP/API failure or missing Bearer.
    pub async fn lookup_status(&self, status_id: &str) -> Result<TwitterHit, TwitterToolError> {
        let id = status_id.trim();
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(TwitterToolError::Other("invalid status id".into()));
        }
        let Some(bearer) = self.creds.bearer.as_ref().filter(|s| !s.is_empty()) else {
            return Err(TwitterToolError::Other(
                "no bearer for status lookup".into(),
            ));
        };
        let client = reqwest::Client::new();
        let url = format!("{TWITTER_API_V2_BASE}/tweets/{id}?tweet.fields=created_at,text");
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
        hit_from_tweet(data, "twitter")
            .ok_or_else(|| TwitterToolError::Other("lookup empty tweet text".into()))
    }

    #[must_use]
    pub const fn creds(&self) -> &TwitterCreds {
        &self.creds
    }

    /// Search / hashtag queries (English-biased). Bearer → X recent search; else PW pulse.
    ///
    /// # Errors
    ///
    /// Returns [`TwitterToolError`] on HTTP/API or pulse script failure.
    pub async fn search(&self, queries: &[String]) -> Result<Vec<TwitterHit>, TwitterToolError> {
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(bearer) = self.creds.bearer.as_ref().filter(|s| !s.is_empty()) {
            return search_recent(bearer, queries).await;
        }
        run_pulse_script("search", queries).await
    }

    /// Home timeline then keyword searches in **one** headed Brave (PW), or two API calls (bearer).
    ///
    /// # Errors
    ///
    /// Returns [`TwitterToolError`] when the combined pulse script fails.
    pub async fn digest_pulse(
        &self,
        queries: &[String],
    ) -> Result<Vec<TwitterHit>, TwitterToolError> {
        if let Some(bearer) = self.creds.bearer.as_ref().filter(|s| !s.is_empty()) {
            let mut out = Vec::new();
            match home_timeline_pulse(bearer).await {
                Ok(hits) => out.extend(hits),
                Err(e) => warn!(error = %e, "twitter: following API failed"),
            }
            match search_recent(bearer, queries).await {
                Ok(hits) => out.extend(hits),
                Err(e) => warn!(error = %e, "twitter: search API failed"),
            }
            return Ok(out);
        }
        run_pulse_script("digest", queries).await
    }

    /// Following / home timeline pulse. Bearer → API; else PW pulse.
    ///
    /// # Errors
    ///
    /// Returns [`TwitterToolError`] on HTTP/API or pulse script failure.
    pub async fn following_pulse(&self) -> Result<Vec<TwitterHit>, TwitterToolError> {
        if let Some(bearer) = self.creds.bearer.as_ref().filter(|s| !s.is_empty()) {
            return home_timeline_pulse(bearer).await;
        }
        run_pulse_script("following", &[]).await
    }
}

async fn run_pulse_script(
    mode: &str,
    queries: &[String],
) -> Result<Vec<TwitterHit>, TwitterToolError> {
    let Some(script) = resolve_twitter_pulse_cmd() else {
        warn!("twitter: no fetch-twitter-pulse.sh; returning empty hits");
        return Ok(Vec::new());
    };
    let mut cmd = Command::new("bash");
    cmd.arg(&script).arg(mode);
    for q in queries
        .iter()
        .take(crate::sources::twitter_queries::MAX_SEARCHES_PER_RUN)
    {
        let q = q.trim();
        if !q.is_empty() {
            cmd.arg(q);
        }
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.env_remove("PLAYWRIGHT_BROWSERS_PATH");
    // Headed Brave is intentional for X; allow override.
    if std::env::var_os("ITCY_TWITTER_HEADLESS").is_none() {
        cmd.env("ITCY_TWITTER_HEADLESS", "0");
    }
    let query_log = queries
        .iter()
        .map(|q| crate::sources::query_for_log(q))
        .collect::<Vec<_>>()
        .join(" | ");
    info!(
        script = %script,
        mode = %mode,
        query = %query_log,
        batch_count = queries.len(),
        "twitter: running PW pulse"
    );
    let output = cmd
        .output()
        .await
        .map_err(|e| TwitterToolError::Other(format!("pulse spawn: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        return Err(TwitterToolError::Other(format!(
            "pulse exit {code}: {}",
            err.chars().take(240).collect::<String>()
        )));
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        info!(mode = %mode, query = %query_log, hits = 0, "twitter: pulse done");
        return Ok(Vec::new());
    }
    let hits: Vec<TwitterHit> = serde_json::from_str(trimmed)
        .map_err(|e| TwitterToolError::Other(format!("pulse json: {e}")))?;
    info!(
        mode = %mode,
        query = %query_log,
        hits = hits.len(),
        "twitter: pulse done"
    );
    Ok(hits)
}

fn ensure_lang_en(query: &str) -> String {
    let q = query.trim();
    if q.to_ascii_lowercase().contains("lang:en") {
        q.to_string()
    } else {
        format!("{q} lang:en")
    }
}

const MAX_HITS_PER_API_SEARCH: usize = 3;

async fn search_recent(
    bearer: &str,
    queries: &[String],
) -> Result<Vec<TwitterHit>, TwitterToolError> {
    let client = reqwest::Client::new();
    let mut hits = Vec::new();
    for q in queries
        .iter()
        .take(crate::sources::twitter_queries::MAX_SEARCHES_PER_RUN)
    {
        let q = ensure_lang_en(q);
        if q.is_empty() {
            continue;
        }
        info!(query = %crate::sources::query_for_log(&q), "twitter: API search");
        let url = format!(
            "{TWITTER_API_V2_BASE}/tweets/search/recent?query={}&max_results=10&tweet.fields=created_at",
            urlencoding_lite(&q)
        );
        let resp = client
            .get(&url)
            .bearer_auth(bearer)
            .send()
            .await
            .map_err(|e| TwitterToolError::Other(format!("search request: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(TwitterToolError::Other(format!(
                "search HTTP {status}: {}",
                body.chars().take(160).collect::<String>()
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| TwitterToolError::Other(format!("search json: {e}")))?;
        let before = hits.len();
        let mut taken = 0usize;
        if let Some(arr) = body.get("data").and_then(|v| v.as_array()) {
            for tw in arr {
                if taken >= MAX_HITS_PER_API_SEARCH {
                    break;
                }
                if let Some(mut hit) = hit_from_tweet(tw, "twitter") {
                    hit.query.clone_from(&q);
                    hits.push(hit);
                    taken += 1;
                }
            }
        }
        info!(
            query = %q,
            hits = hits.len().saturating_sub(before),
            "twitter: API search done"
        );
    }
    Ok(hits)
}

async fn home_timeline_pulse(bearer: &str) -> Result<Vec<TwitterHit>, TwitterToolError> {
    let client = reqwest::Client::new();
    let me = client
        .get(format!("{TWITTER_API_V2_BASE}/users/me"))
        .bearer_auth(bearer)
        .send()
        .await
        .map_err(|e| TwitterToolError::Other(format!("users/me: {e}")))?;
    if !me.status().is_success() {
        let status = me.status();
        let body = me.text().await.unwrap_or_default();
        return Err(TwitterToolError::Other(format!(
            "users/me HTTP {status}: {}",
            body.chars().take(160).collect::<String>()
        )));
    }
    let me_json: serde_json::Value = me
        .json()
        .await
        .map_err(|e| TwitterToolError::Other(format!("users/me json: {e}")))?;
    let uid = me_json
        .pointer("/data/id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TwitterToolError::Other("users/me missing data.id".into()))?;
    let url = format!(
        "{TWITTER_API_V2_BASE}/users/{uid}/timelines/reverse_chronological?max_results=20&tweet.fields=created_at"
    );
    let resp = client
        .get(&url)
        .bearer_auth(bearer)
        .send()
        .await
        .map_err(|e| TwitterToolError::Other(format!("timeline: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(TwitterToolError::Other(format!(
            "timeline HTTP {status}: {}",
            body.chars().take(160).collect::<String>()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| TwitterToolError::Other(format!("timeline json: {e}")))?;
    let mut hits = Vec::new();
    if let Some(arr) = body.get("data").and_then(|v| v.as_array()) {
        for tw in arr {
            if let Some(hit) = hit_from_tweet(tw, "following") {
                hits.push(hit);
            }
        }
    }
    Ok(hits)
}

fn hit_from_tweet(tw: &serde_json::Value, lane_hint: &str) -> Option<TwitterHit> {
    let id = tw.get("id")?.as_str()?;
    let text = tw.get("text")?.as_str().unwrap_or("").trim();
    if text.is_empty() {
        return None;
    }
    let subject: String = text
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    Some(TwitterHit {
        title: text.to_string(),
        url: format!("https://x.com/i/web/status/{id}"),
        subject: if subject.is_empty() {
            lane_hint.into()
        } else {
            subject
        },
        detail: text.to_string(),
        lane: if lane_hint == "following" {
            "following".into()
        } else {
            "twitter".into()
        },
        query: String::new(),
    })
}

fn urlencoding_lite(q: &str) -> String {
    let mut out = String::new();
    for b in q.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_value() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".twitter");
        std::fs::write(
            &path,
            "TWITTER_USER=interchouette\nTWITTER_BEARER=tok_abc\n",
        )
        .unwrap();
        let c = load_twitter_creds_from(&path).unwrap();
        assert_eq!(c.user.as_deref(), Some("interchouette"));
        assert_eq!(c.bearer.as_deref(), Some("tok_abc"));
        assert!(c.status_line().contains("bearer"));
    }

    #[test]
    fn parses_two_line_legacy() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".twitter");
        std::fs::write(&path, "interchouette\nsecretsecretsecretsecretsecretse\n").unwrap();
        let c = load_twitter_creds_from(&path).unwrap();
        assert_eq!(c.user.as_deref(), Some("interchouette"));
        assert!(c.has_secret());
    }

    #[test]
    fn ensure_lang_en_appends_once() {
        assert_eq!(ensure_lang_en("#rust"), "#rust lang:en");
        assert_eq!(ensure_lang_en("#rust lang:en"), "#rust lang:en");
    }

    #[test]
    fn parses_pulse_json_hit() {
        let raw = r#"[{"title":"Hello Rust","url":"https://x.com/a/status/1","subject":"Hello Rust","detail":"full tweet body here"}]"#;
        let hits: Vec<TwitterHit> = serde_json::from_str(raw).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://x.com/a/status/1");
        assert_eq!(hits[0].detail, "full tweet body here");
        assert_eq!(hits[0].lane, "twitter");
    }

    #[test]
    fn hit_from_tweet_keeps_full_text() {
        let long = "a".repeat(180);
        let tw = serde_json::json!({ "id": "99", "text": long });
        let hit = hit_from_tweet(&tw, "q").unwrap();
        assert_eq!(hit.title.chars().count(), 180);
        assert_eq!(hit.detail, hit.title);
        assert_eq!(hit.lane, "twitter");
    }

    #[test]
    fn hit_from_tweet_marks_following_lane() {
        let tw = serde_json::json!({ "id": "1", "text": "hello from the home timeline today" });
        let hit = hit_from_tweet(&tw, "following").unwrap();
        assert_eq!(hit.lane, "following");
    }
}

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Public URL ingest (HTTP first; optional public Playwright enrichment).

use crate::sources::embed::{default_embed_model, EmbedClient};
use crate::sources::export::chunk_text;
use crate::sources::html::{content_preview, extract_page_text, html_to_text, infer_subject};
use crate::sources::store::{InsertSource, SourceDb};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::process::Command;
use tracing::{info, warn};

/// Below this after page extract → try public Playwright (if configured / default script).
pub const THIN_TRIGGER_CHARS: usize = 800;

/// Absolute minimum to store in corpus after HTTP and optional PW.
pub const MIN_STORE_CHARS: usize = 200;

/// Transient gateway / Cloudflare origin timeouts (retry a few times).
const TRANSIENT_HTTP_ATTEMPTS: u32 = 3;
const TRANSIENT_HTTP_GAP: Duration = Duration::from_secs(2);

/// Errors during URL ingest.
#[derive(Debug, Error)]
pub enum IngestError {
    #[error("fetch: {0}")]
    Fetch(String),
    #[error("store: {0}")]
    Store(String),
    #[error("embed: {0}")]
    Embed(String),
    #[error("content too thin after public fetch ({0} chars)")]
    TooThin(usize),
}

/// How HTML was obtained for this ingest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestFetchPath {
    Http,
    PublicPlaywright,
}

impl IngestFetchPath {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "HTTP GET (reqwest)",
            Self::PublicPlaywright => "public Playwright (scripts/fetch-public-page.sh)",
        }
    }
}

/// Operator-facing ingest result (Slack + logs).
#[derive(Debug, Clone)]
pub struct IngestReport {
    pub source_id: i64,
    pub subject: String,
    pub title: String,
    pub url: String,
    pub chars: usize,
    pub chunks: usize,
    pub embed_model: String,
    pub fetch_path: IngestFetchPath,
    pub preview: String,
    pub replaced: bool,
}

impl IngestReport {
    #[must_use]
    pub fn slack_message(&self) -> String {
        let replaced = if self.replaced {
            "updated existing row"
        } else {
            "new row"
        };
        format!(
            "*Ingest complete*\n\
• source `#{id}` ({replaced})\n\
• subject: `{subject}`\n\
• title: {title}\n\
• fetch: {fetch}\n\
• text: {chars} chars · {chunks} chunks · embed `{model}`\n\
• preview: {preview}\n\
`{url}`",
            id = self.source_id,
            replaced = replaced,
            subject = self.subject,
            title = self.title,
            fetch = self.fetch_path.as_str(),
            chars = self.chars,
            chunks = self.chunks,
            model = self.embed_model,
            preview = self.preview,
            url = self.url,
        )
    }
}

/// Fetches public page HTML (no login).
#[async_trait]
pub trait PageFetcher: Send + Sync {
    async fn fetch_html(&self, url: &str) -> Result<String, IngestError>;
}

/// Plain HTTP GET.
pub struct HttpPageFetcher {
    http: reqwest::Client,
}

impl HttpPageFetcher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent("ITCy/0.1 (+https://interchouette.net; public ingest)")
                .timeout(Duration::from_secs(25))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }
}

impl Default for HttpPageFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PageFetcher for HttpPageFetcher {
    async fn fetch_html(&self, url: &str) -> Result<String, IngestError> {
        let mut last_err = String::new();
        for attempt in 1..=TRANSIENT_HTTP_ATTEMPTS {
            match self.fetch_html_once(url).await {
                Ok(body) => return Ok(body),
                Err(IngestError::Fetch(msg)) => {
                    let transient = is_transient_fetch_err(&msg);
                    last_err = msg;
                    if !transient || attempt == TRANSIENT_HTTP_ATTEMPTS {
                        break;
                    }
                    warn!(
                        url = %url,
                        attempt,
                        error = %last_err,
                        "ingest: transient HTTP; retrying"
                    );
                    tokio::time::sleep(TRANSIENT_HTTP_GAP).await;
                }
                Err(e) => return Err(e),
            }
        }
        Err(IngestError::Fetch(last_err))
    }
}

impl HttpPageFetcher {
    async fn fetch_html_once(&self, url: &str) -> Result<String, IngestError> {
        let res = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| IngestError::Fetch(e.to_string()))?;
        let status = res.status();
        let body = res
            .text()
            .await
            .map_err(|e| IngestError::Fetch(e.to_string()))?;
        if !status.is_success() {
            return Err(IngestError::Fetch(format_http_status(status)));
        }
        Ok(body)
    }
}

fn format_http_status(status: reqwest::StatusCode) -> String {
    // Cloudflare 52x are not in the IANA table; Display adds "<unknown status code>".
    format!("HTTP {}", status.as_u16())
}

fn is_transient_fetch_err(msg: &str) -> bool {
    const CODES: &[&str] = &[
        "HTTP 408", "HTTP 425", "HTTP 429", "HTTP 500", "HTTP 502", "HTTP 503", "HTTP 504",
        "HTTP 520", "HTTP 521", "HTTP 522", "HTTP 523", "HTTP 524",
    ];
    CODES.iter().any(|c| msg.contains(c))
        || msg.contains("timed out")
        || msg.contains("timeout")
        || msg.contains("connection reset")
        || msg.contains("connection closed")
}

/// HTTP statuses where a headless browser may succeed (bot walls, rate limits).
#[must_use]
pub(crate) const fn is_bot_wall_http_status(status: u16) -> bool {
    matches!(status, 401 | 403 | 429)
}

/// Parse [`HttpPageFetcher`] errors for [`is_bot_wall_http_status`].
#[must_use]
pub(crate) fn is_bot_wall_fetch_err(msg: &str) -> bool {
    is_bot_wall_http_status(401) && msg.contains("HTTP 401")
        || is_bot_wall_http_status(403) && msg.contains("HTTP 403")
        || is_bot_wall_http_status(429) && msg.contains("HTTP 429")
}

/// HTTP then optional public Playwright when extracted text is thin.
///
/// Default command: `scripts/fetch-public-page.sh` when present under cwd / repo.
/// Override with `ITCY_PUBLIC_FETCH_CMD`. Never for logged-in `LinkedIn`.
pub struct HttpThenPublicPlaywright;

impl HttpThenPublicPlaywright {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for HttpThenPublicPlaywright {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PageFetcher for HttpThenPublicPlaywright {
    async fn fetch_html(&self, url: &str) -> Result<String, IngestError> {
        let (html, _) = fetch_public_page_html(url).await?;
        Ok(html)
    }
}

/// Resolves shell command for public PW fetch (env or default script path).
#[must_use]
pub fn resolve_public_fetch_cmd() -> Option<String> {
    if let Ok(raw) = std::env::var("ITCY_PUBLIC_FETCH_CMD") {
        let t = raw.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    for candidate in [
        PathBuf::from("scripts/fetch-public-page.sh"),
        PathBuf::from("../scripts/fetch-public-page.sh"),
        crate::paths::product_join("scripts/fetch-public-page.sh"),
    ] {
        if candidate.is_file() {
            return Some(candidate.display().to_string());
        }
    }
    None
}

pub(crate) async fn try_public_playwright_fetch(url: &str) -> Result<Option<String>, IngestError> {
    let Some(cmd_tmpl) = resolve_public_fetch_cmd() else {
        warn!("ingest: no ITCY_PUBLIC_FETCH_CMD / fetch-public-page.sh; staying on HTTP HTML");
        return Ok(None);
    };
    let cmdline = if cmd_tmpl.contains("{}") {
        cmd_tmpl.replace("{}", url)
    } else {
        format!("{cmd_tmpl} {url}")
    };
    info!(cmd = %cmdline, url = %url, "ingest: running public Playwright fetch cmd");
    let mut command = Command::new("bash");
    command
        .arg("-lc")
        .arg(&cmdline)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in public_fetch_subprocess_env() {
        command.env(key, value);
    }
    if std::env::var("ITCY_ROOT").is_err() {
        command.env("ITCY_ROOT", crate::paths::product_root());
    }
    let output = command
        .output()
        .await
        .map_err(|e| IngestError::Fetch(format!("playwright public cmd: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(IngestError::Fetch(format!(
            "playwright public cmd failed: {err}"
        )));
    }
    let html = String::from_utf8_lossy(&output.stdout).to_string();
    if html.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(html))
}

/// Env vars forwarded to [`try_public_playwright_fetch`] (`fetch-public-page.sh`).
#[must_use]
pub fn public_fetch_subprocess_env() -> Vec<(String, String)> {
    const KEYS: &[&str] = &[
        "ITCY_PW_BROWSER",
        "ITCY_BROWSER_EXECUTABLE",
        "ITCY_OBSCURA_CDP_PORT",
        "ITCY_OBSCURA_STEALTH",
        "ITCY_PW_USER_DATA_DIR",
        "ITCY_PUBLIC_FETCH_HEADED",
        "ITCY_PUBLIC_FETCH_CF_WAIT_MS",
        "ITCY_ROOT",
        "ITCY_OBSCURA_CDP_URL",
    ];
    KEYS.iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .map(|value| ((*key).to_string(), value))
        })
        .collect()
}

async fn public_playwright_or_http_err(url: &str, http_err: &str) -> Result<String, IngestError> {
    match try_public_playwright_fetch(url).await {
        Ok(Some(html)) => {
            info!(url = %url, "ingest: public Playwright ok after HTTP bot wall");
            Ok(html)
        }
        Ok(None) => Err(IngestError::Fetch(http_err.to_string())),
        Err(e) => Err(e),
    }
}

fn reject_cloudflare_challenge(url: &str, html: &str) -> Result<(), IngestError> {
    if crate::sources::html::looks_like_cloudflare_challenge(html) {
        warn!(url = %url, "ingest: Cloudflare challenge page after fetch");
        return Err(IngestError::Fetch(
            "Cloudflare bot check (automated fetch could not pass). \
Use a PMC/DOI mirror, GitHub/README cite, or set ITCY_PUBLIC_FETCH_HEADED=1 and open the URL once in Brave."
                .into(),
        ));
    }
    Ok(())
}

fn is_loopback_url(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.contains("127.0.0.1") || u.contains("localhost") || u.contains("[::1]")
}

/// HTTP then optional Playwright (`fetch-public-page.sh`). Shared by `/ingest` and cite probes.
///
/// # Errors
///
/// Returns [`IngestError::Fetch`] on network failure, bot wall after Playwright, or Cloudflare shell.
pub async fn fetch_public_page_html(url: &str) -> Result<(String, IngestFetchPath), IngestError> {
    let http = HttpPageFetcher::new();
    let (html, path) = match http.fetch_html(url).await {
        Ok(body) => {
            let chars = extract_page_text(&body).chars().count();
            let needs_pw = !is_loopback_url(url)
                && (chars < THIN_TRIGGER_CHARS
                    || crate::sources::html::looks_like_cloudflare_challenge(&body));
            if !needs_pw {
                info!(url = %url, chars, "ingest: HTTP GET ok (no Playwright)");
                return Ok((body, IngestFetchPath::Http));
            }
            warn!(
                url = %url,
                chars,
                trigger = THIN_TRIGGER_CHARS,
                "ingest: HTTP thin or bot shell; trying public Playwright if available"
            );
            match try_public_playwright_fetch(url).await {
                Ok(Some(pw)) => {
                    let pw_chars = extract_page_text(&pw).chars().count();
                    if pw_chars > chars {
                        info!(url = %url, http_chars = chars, pw_chars, "ingest: public Playwright enrichment ok");
                        (pw, IngestFetchPath::PublicPlaywright)
                    } else {
                        warn!(
                            url = %url,
                            http_chars = chars,
                            pw_chars,
                            "ingest: public Playwright not richer than HTTP; keeping HTTP HTML"
                        );
                        (body, IngestFetchPath::Http)
                    }
                }
                Ok(None) => (body, IngestFetchPath::Http),
                Err(e) => {
                    warn!(url = %url, error = %e, "ingest: public Playwright failed; keeping HTTP HTML");
                    (body, IngestFetchPath::Http)
                }
            }
        }
        Err(IngestError::Fetch(msg)) if is_bot_wall_fetch_err(&msg) => {
            warn!(url = %url, error = %msg, "ingest: HTTP bot wall; trying public Playwright");
            let pw = public_playwright_or_http_err(url, &msg).await?;
            (pw, IngestFetchPath::PublicPlaywright)
        }
        Err(e) => return Err(e),
    };
    reject_cloudflare_challenge(url, &html)?;
    Ok((html, path))
}

/// Ingests a public URL into the source DB with embeddings (upsert by URL).
///
/// HTTP first; if page extract is under [`THIN_TRIGGER_CHARS`], runs default
/// `scripts/fetch-public-page.sh` (or `ITCY_PUBLIC_FETCH_CMD`).
///
/// # Errors
///
/// Returns [`IngestError::TooThin`] when extracted text is too short, or another [`IngestError`] for fetch/store failure.
pub async fn ingest_url(
    url: &str,
    db_path: &Path,
    embed: &dyn EmbedClient,
    _fetcher: &dyn PageFetcher,
) -> Result<IngestReport, IngestError> {
    info!(url = %url, "ingest: start");
    let (html, path) = fetch_public_page_html(url).await?;
    ingest_html(url, db_path, embed, &html, path).await
}

/// Test / inject entry: ingest already-fetched HTML.
///
/// # Errors
///
/// Returns [`IngestError::TooThin`] when extracted text is too short, or another [`IngestError`] for fetch/store failure.
pub async fn ingest_html(
    url: &str,
    db_path: &Path,
    embed: &dyn EmbedClient,
    html: &str,
    fetch_path: IngestFetchPath,
) -> Result<IngestReport, IngestError> {
    let text = extract_page_text(html);
    let chars = text.chars().count();
    if chars < MIN_STORE_CHARS {
        warn!(url = %url, chars, "ingest: content too thin");
        return Err(IngestError::TooThin(chars));
    }
    let title = extract_title(html).unwrap_or_else(|| {
        let preview: String = text.chars().take(80).collect();
        preview
    });
    let subject = infer_subject(&title, &text);
    let preview = content_preview(&text, 320);
    info!(
        url = %url,
        chars,
        title = %title.chars().take(120).collect::<String>(),
        subject = %subject,
        fetch = fetch_path.as_str(),
        "ingest: extracted page text"
    );
    let model = default_embed_model();
    let mut prepared: Vec<(String, Vec<f32>)> = Vec::new();
    let chunks: Vec<String> = chunk_text(&text, 800);
    info!(
        url = %url,
        model = %model,
        n_chunks = chunks.len(),
        "ingest: embedding via Ollama"
    );
    for chunk in &chunks {
        let embedding = embed
            .embed(&model, chunk)
            .await
            .map_err(|e| IngestError::Embed(e.to_string()))?;
        prepared.push((chunk.clone(), embedding));
    }
    let db = SourceDb::open(db_path).map_err(|e| IngestError::Store(e.to_string()))?;
    let (id, replaced) = store_ingested(&db, url, &subject, &title, &text, &prepared)?;
    info!(
        url = %url,
        source_id = id,
        subject = %subject,
        chunks = prepared.len(),
        replaced,
        "ingest: stored in corpus (same runtime.db as enrich / draft RAG)"
    );
    Ok(IngestReport {
        source_id: id,
        subject,
        title,
        url: url.to_string(),
        chars,
        chunks: prepared.len(),
        embed_model: model,
        fetch_path,
        preview,
        replaced,
    })
}

fn store_ingested(
    db: &SourceDb,
    url: &str,
    subject: &str,
    title: &str,
    text: &str,
    prepared: &[(String, Vec<f32>)],
) -> Result<(i64, bool), IngestError> {
    db.with_transaction(|conn| {
        let existing = SourceDb::find_source_id_by_url_on(conn, url)?;
        let (source_id, replaced) = if let Some(id) = existing {
            SourceDb::delete_chunks_for_source_on(conn, id)?;
            SourceDb::update_url_ingest_on(conn, id, subject, title, text)?;
            (id, true)
        } else {
            let id = SourceDb::insert_source_on(
                conn,
                &InsertSource {
                    kind: "url",
                    activity: "url",
                    subject,
                    title,
                    url: Some(url),
                    raw_text: text,
                    occurred_at: None,
                },
            )?
            .ok_or_else(|| {
                crate::sources::store::SourceError::Other(format!(
                    "url ingest insert conflict: {url}"
                ))
            })?;
            (id, false)
        };
        for (chunk, embedding) in prepared {
            SourceDb::insert_chunk_on(conn, source_id, subject, chunk, embedding)?;
        }
        Ok((source_id, replaced))
    })
    .map_err(|e| IngestError::Store(e.to_string()))
}

fn extract_title(html: &str) -> Option<String> {
    if let Some(og) = meta_property_content(html, "og:title") {
        let t = og.trim();
        if !t.is_empty() && !t.eq_ignore_ascii_case("medium") {
            return Some(t.to_string());
        }
    }
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title>")? + 7;
    let end = lower[start..].find("</title>")? + start;
    let raw = html_to_text(&html[start..end]);
    let t = raw.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn meta_property_content(html: &str, prop: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let needle = format!("property=\"{prop}\"");
    let idx = lower.find(&needle)?;
    let window = &html[idx..].chars().take(400).collect::<String>();
    let window_l = window.to_ascii_lowercase();
    let cidx = window_l.find("content=\"")?;
    let rest = &window[cidx + 9..];
    let end = rest.find('"')?;
    Some(html_to_text(&rest[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::embed::MockEmbedClient;
    use tempfile::TempDir;

    #[tokio::test]
    async fn ingest_stores_url_chunks() {
        let dir = TempDir::new().expect("temp");
        let db_path = dir.path().join("s.db");
        let html = format!(
            "<html><title>Rust Tips</title><body>{}</body></html>",
            "Rust async tokio ".repeat(40)
        );
        let report = ingest_html(
            "https://example.com/rust",
            &db_path,
            &MockEmbedClient,
            &html,
            IngestFetchPath::Http,
        )
        .await
        .expect("ingest");
        assert!(report.source_id > 0);
        assert!(!report.subject.is_empty());
        assert!(report.chars >= MIN_STORE_CHARS);
        assert!(report.slack_message().contains("Ingest complete"));
        let db = SourceDb::open(&db_path).expect("db");
        let chunks = db
            .get_chunk_candidates(&report.subject, 10)
            .expect("chunks");
        assert!(!chunks.is_empty());
    }

    #[tokio::test]
    async fn thin_html_errors() {
        let dir = TempDir::new().expect("temp");
        let db_path = dir.path().join("s.db");
        let err = ingest_html(
            "https://example.com/x",
            &db_path,
            &MockEmbedClient,
            "<html><body>hi</body></html>",
            IngestFetchPath::Http,
        )
        .await
        .expect_err("thin");
        assert!(matches!(err, IngestError::TooThin(_)));
    }

    #[tokio::test]
    async fn upsert_replaces_same_url() {
        let dir = TempDir::new().expect("temp");
        let db_path = dir.path().join("s.db");
        let html1 = format!(
            "<html><title>One</title><article>{}</article></html>",
            "alpha signal body ".repeat(40)
        );
        let html2 = format!(
            "<html><title>Two</title><article>{}</article></html>",
            "beta signal body ".repeat(40)
        );
        let a = ingest_html(
            "https://example.com/same",
            &db_path,
            &MockEmbedClient,
            &html1,
            IngestFetchPath::Http,
        )
        .await
        .expect("a");
        let b = ingest_html(
            "https://example.com/same",
            &db_path,
            &MockEmbedClient,
            &html2,
            IngestFetchPath::Http,
        )
        .await
        .expect("b");
        assert_eq!(a.source_id, b.source_id);
        assert!(b.replaced);
        assert!(b.preview.to_ascii_lowercase().contains("beta"));
    }

    #[test]
    fn public_fetch_cmd_resolves_repo_script() {
        let cmd = resolve_public_fetch_cmd();
        assert!(
            cmd.is_some(),
            "expected scripts/fetch-public-page.sh under product root"
        );
    }

    #[test]
    fn bot_wall_fetch_err_detects_403_for_playwright_fallback() {
        assert!(is_bot_wall_fetch_err("HTTP 403"));
        assert!(is_bot_wall_fetch_err("fetch: HTTP 403"));
        assert!(!is_bot_wall_fetch_err("HTTP 404"));
        assert!(is_bot_wall_http_status(403));
        assert!(!is_bot_wall_http_status(404));
    }

    #[test]
    fn cloudflare_challenge_rejected_after_fetch() {
        let cf = r#"<html><head><title>Just a moment...</title></head>
<body><script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script></body></html>"#;
        let err = reject_cloudflare_challenge("https://example.com/x", cf).expect_err("cf");
        assert!(err.to_string().contains("Cloudflare"));
    }

    #[test]
    fn transient_fetch_detects_cloudflare_522() {
        assert!(is_transient_fetch_err("HTTP 522"));
        assert!(is_transient_fetch_err("HTTP 524"));
        assert!(!is_transient_fetch_err("HTTP 404"));
        assert!(!is_transient_fetch_err("HTTP 403"));
    }

    #[test]
    fn public_fetch_subprocess_env_forwards_browser_vars() {
        // SAFETY: test serializes env access; no other threads read these vars here.
        unsafe {
            std::env::set_var("ITCY_PW_BROWSER", "obscura");
            std::env::set_var("ITCY_BROWSER_EXECUTABLE", "/tmp/obscura-test-bin");
            std::env::set_var("ITCY_OBSCURA_CDP_PORT", "9223");
        }
        let pairs = public_fetch_subprocess_env();
        unsafe {
            std::env::remove_var("ITCY_PW_BROWSER");
            std::env::remove_var("ITCY_BROWSER_EXECUTABLE");
            std::env::remove_var("ITCY_OBSCURA_CDP_PORT");
        }
        let map: std::collections::HashMap<_, _> = pairs.into_iter().collect();
        assert_eq!(
            map.get("ITCY_PW_BROWSER").map(String::as_str),
            Some("obscura")
        );
        assert_eq!(
            map.get("ITCY_BROWSER_EXECUTABLE").map(String::as_str),
            Some("/tmp/obscura-test-bin")
        );
        assert_eq!(
            map.get("ITCY_OBSCURA_CDP_PORT").map(String::as_str),
            Some("9223")
        );
        assert!(!map.contains_key("ITCY_OBSCURA_STEALTH"));
    }

    #[test]
    fn http_status_formats_numeric_only() {
        let s = reqwest::StatusCode::from_u16(522).expect("522");
        assert_eq!(format_http_status(s), "HTTP 522");
    }
}

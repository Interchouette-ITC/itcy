// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Tor-backed `LinkedIn` public URL enrich for link-only post/repost stubs.
//!
//! Own binary `enrich-linkedin-urls`. Progress lives in `SQLite` (`enrich_status`).
//! HTTP GET via `socks5h://127.0.0.1:9050` only (no Playwright, no clearnet).

use crate::sources::embed::{default_embed_model, EmbedClient};
use crate::sources::export::chunk_text;
use crate::sources::html::infer_subject;
use crate::sources::linkedin_extract::extract_linkedin_public_post;
use crate::sources::scrape_cache::{ScrapeCache, ScrapePage};
use crate::sources::store::SourceDb;
use async_trait::async_trait;
use chrono::{Duration, Local};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;
use thiserror::Error;
use tracing::{error, info, warn};

/// Default Tor SOCKS (DNS through proxy).
pub const DEFAULT_TOR_SOCKS: &str = "socks5h://127.0.0.1:9050";
/// Default Tor `ControlPort` for NEWNYM.
pub const DEFAULT_TOR_CONTROL: &str = "127.0.0.1:9051";
/// Base delay between drip attempts.
pub const BASE_DELAY: StdDuration = StdDuration::from_mins(20);
/// Max jitter added to base delay (0..=10 min).
pub const JITTER_MAX_SECS: u64 = 10 * 60;
/// Stale `in_flight` reclaim window (laptop died mid-GET).
pub const STALE_CLAIM: StdDuration = StdDuration::from_mins(45);
/// Wall / auth backoff (per-source; loop keeps dripping other claims).
pub const WALL_BACKOFF: StdDuration = StdDuration::from_hours(6);
/// Hard Tor HTTP (403/401/404/5xx): same long backoff as wall, no `wall_streak`.
pub const HARD_HTTP_BACKOFF: StdDuration = StdDuration::from_hours(6);
/// Wait after `SIGNAL NEWNYM` before the next Tor GET (circuit build).
pub const NEWNYM_SETTLE: StdDuration = StdDuration::from_secs(10);
/// Minimum chars used only to *detect* export stubs for the queue (not extract accept).
pub const MIN_ENRICH_CHARS: usize = 120;
/// Discrete browser-like UA (no `ITCy` banner).
pub const ENRICH_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; rv:128.0) Gecko/20100101 Firefox/128.0";

/// Errors from enrich loop.
#[derive(Debug, Error)]
pub enum EnrichError {
    #[error("enrich store: {0}")]
    Store(String),
    #[error("enrich fetch: {0}")]
    Fetch(String),
    #[error("enrich embed: {0}")]
    Embed(String),
    #[error("enrich tor: {0}")]
    Tor(String),
    #[error("enrich lock: {0}")]
    Lock(String),
    #[error("enrich: {0}")]
    Other(String),
}

/// Fetches HTML through Tor only.
#[async_trait]
pub trait TorPageFetcher: Send + Sync {
    async fn fetch_html(&self, url: &str) -> Result<String, EnrichError>;
}

/// Reqwest client forced through `SOCKS5h`.
pub struct TorSocksFetcher {
    http: reqwest::Client,
}

impl TorSocksFetcher {
    /// Builds a fail-closed Tor client (`socks5h://…`).
    ///
    /// # Errors
    ///
    /// Returns [`EnrichError::Tor`] when the SOCKS proxy URL or HTTP client cannot be built.
    pub fn new(socks_url: &str) -> Result<Self, EnrichError> {
        let proxy = reqwest::Proxy::all(socks_url)
            .map_err(|e| EnrichError::Tor(format!("proxy {socks_url}: {e}")))?;
        let http = reqwest::Client::builder()
            .proxy(proxy)
            .user_agent(ENRICH_USER_AGENT)
            .timeout(StdDuration::from_secs(90))
            .build()
            .map_err(|e| EnrichError::Tor(format!("client: {e}")))?;
        Ok(Self { http })
    }

    async fn fetch_html_once(&self, url: &str) -> Result<String, EnrichError> {
        let res = self
            .http
            .get(url)
            // Tor exits often truncate mid-body; identity avoids compressed-stream decode fails.
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .header(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8",
            )
            .send()
            .await
            .map_err(|e| EnrichError::Fetch(e.to_string()))?;
        let status = res.status();
        // bytes + lossy UTF-8: stream/charset edge cases must not kill a usable HTML page.
        let raw = res
            .bytes()
            .await
            .map_err(|e| EnrichError::Fetch(e.to_string()))?;
        let body = String::from_utf8_lossy(&raw).into_owned();
        if status.as_u16() == 429 || status.as_u16() == 999 {
            return Err(EnrichError::Fetch(format!("wall HTTP {status}")));
        }
        if !status.is_success() {
            // 403/4xx/5xx from Tor exit are hard fetch failures (not soft walls).
            error!(%status, url = %url_host_path(url), "enrich Tor GET failed");
            return Err(EnrichError::Fetch(format!("HTTP {status}")));
        }
        if body.to_ascii_lowercase().contains("authwall")
            || body.to_ascii_lowercase().contains("challenge")
                && body.to_ascii_lowercase().contains("linkedin")
        {
            // Soft signal; thin text check still applies.
            warn!(%status, "linkedin page may be gated");
        }
        Ok(body)
    }
}

/// Tor circuit / stream flakes (same claim retries once).
#[must_use]
pub fn is_transient_tor_fetch(err: &EnrichError) -> bool {
    let s = err.to_string().to_ascii_lowercase();
    s.contains("error sending request")
        || s.contains("error decoding response body")
        || s.contains("timed out")
        || s.contains("timeout")
        || s.contains("connection reset")
        || s.contains("connection closed")
        || s.contains("unexpected eof")
        || s.contains("broken pipe")
}

#[async_trait]
impl TorPageFetcher for TorSocksFetcher {
    async fn fetch_html(&self, url: &str) -> Result<String, EnrichError> {
        match self.fetch_html_once(url).await {
            Ok(body) => Ok(body),
            Err(e) if is_transient_tor_fetch(&e) => {
                warn!(
                    error = %e,
                    url = %url_host_path(url),
                    "transient Tor fetch - retrying once"
                );
                tokio::time::sleep(StdDuration::from_secs(3)).await;
                self.fetch_html_once(url).await
            }
            Err(e) => Err(e),
        }
    }
}

/// In-memory fetcher for unit tests.
pub struct MockTorFetcher {
    pub pages: std::collections::HashMap<String, String>,
}

#[async_trait]
impl TorPageFetcher for MockTorFetcher {
    async fn fetch_html(&self, url: &str) -> Result<String, EnrichError> {
        self.pages
            .get(url)
            .cloned()
            .ok_or_else(|| EnrichError::Fetch(format!("mock miss: {url}")))
    }
}

/// True when export left a link stub (or very short body).
#[must_use]
pub fn is_link_stub(raw_text: &str) -> bool {
    let t = raw_text.trim();
    t.starts_with("Post") || t.starts_with("Repost") || t.chars().count() < MIN_ENRICH_CHARS
}

/// PID file lock so only one enrich process runs.
#[derive(Debug)]
pub struct PidLock {
    path: PathBuf,
}

impl PidLock {
    /// Acquires lock or errors if another live process holds it.
    ///
    /// # Errors
    ///
    /// Returns an [`EnrichError`] variant describing claim, fetch, wall, or store failure.
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self, EnrichError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| EnrichError::Lock(e.to_string()))?;
        }
        if path.exists() {
            if let Ok(contents) = fs::read_to_string(&path) {
                if let Ok(pid) = contents.trim().parse::<i32>() {
                    if process_alive(pid) {
                        return Err(EnrichError::Lock(format!(
                            "enrich already running (pid {pid}, {})",
                            path.display()
                        )));
                    }
                }
            }
        }
        fs::write(&path, format!("{}\n", std::process::id()))
            .map_err(|e| EnrichError::Lock(e.to_string()))?;
        Ok(Self { path })
    }
}

impl Drop for PidLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// True when `/proc/{pid}` exists (Linux). Used by PID lock and `/status`.
#[must_use]
pub fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Best-effort enrich drip side signals next to the state DB (`sql/`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnrichSideSignals {
    pub wall_streak: Option<u32>,
    pub last_wall_source_id: Option<i64>,
    pub enrich_pid: Option<i32>,
    pub enrich_running: bool,
}

/// Reads `enrich-wall-streak.txt` + `enrich-linkedin-urls.pid` beside `state_db_path`.
#[must_use]
pub fn read_enrich_side_signals(state_db_path: &Path) -> EnrichSideSignals {
    let dir = state_db_path.parent().unwrap_or_else(|| Path::new("."));
    let mut out = EnrichSideSignals::default();

    let streak_path = dir.join("enrich-wall-streak.txt");
    if let Ok(raw) = fs::read_to_string(&streak_path) {
        for line in raw.lines() {
            if let Some(v) = line.strip_prefix("wall_streak=") {
                out.wall_streak = v.trim().parse().ok();
            } else if let Some(v) = line.strip_prefix("last_wall_source_id=") {
                let t = v.trim();
                if !t.is_empty() {
                    out.last_wall_source_id = t.parse().ok();
                }
            }
        }
    }

    let pid_path = dir.join("enrich-linkedin-urls.pid");
    if let Ok(raw) = fs::read_to_string(&pid_path) {
        if let Ok(pid) = raw.trim().parse::<i32>() {
            out.enrich_pid = Some(pid);
            out.enrich_running = process_alive(pid);
        }
    }
    out
}

/// Sends Tor `ControlPort` `SIGNAL NEWNYM` (new circuit).
///
/// # Errors
///
/// Returns an [`EnrichError`] variant describing claim, fetch, wall, or store failure.
pub fn tor_newnym(control_addr: &str) -> Result<(), EnrichError> {
    let mut stream = TcpStream::connect(control_addr)
        .map_err(|e| EnrichError::Tor(format!("control connect {control_addr}: {e}")))?;
    stream
        .set_read_timeout(Some(StdDuration::from_secs(10)))
        .ok();
    stream
        .set_write_timeout(Some(StdDuration::from_secs(10)))
        .ok();
    stream
        .write_all(b"AUTHENTICATE\r\n")
        .map_err(|e| EnrichError::Tor(format!("auth write: {e}")))?;
    let mut buf = [0u8; 256];
    let n = stream
        .read(&mut buf)
        .map_err(|e| EnrichError::Tor(format!("auth read: {e}")))?;
    let reply = String::from_utf8_lossy(&buf[..n]);
    if !reply.contains("250") {
        return Err(EnrichError::Tor(format!("AUTHENTICATE failed: {reply}")));
    }
    stream
        .write_all(b"SIGNAL NEWNYM\r\n")
        .map_err(|e| EnrichError::Tor(format!("newnym write: {e}")))?;
    let n = stream
        .read(&mut buf)
        .map_err(|e| EnrichError::Tor(format!("newnym read: {e}")))?;
    let reply = String::from_utf8_lossy(&buf[..n]);
    if !reply.contains("250") {
        return Err(EnrichError::Tor(format!("NEWNYM failed: {reply}")));
    }
    let _ = stream.write_all(b"QUIT\r\n");
    Ok(())
}

/// Probe one `LinkedIn` URL through Tor; fail closed if blocked/thin.
///
/// # Errors
///
/// Returns an [`EnrichError`] variant describing claim, fetch, wall, or store failure.
pub async fn probe_tor_linkedin(
    fetcher: &dyn TorPageFetcher,
    url: &str,
) -> Result<(), EnrichError> {
    let html = fetcher.fetch_html(url).await?;
    let extracted = extract_linkedin_public_post(&html);
    if !extracted.ok {
        return Err(EnrichError::Tor(format!(
            "probe extract failed ({}) - LinkedIn may be blocking Tor",
            extracted.reason
        )));
    }
    Ok(())
}

/// Housekeeping before drip: purge reactions, queue stubs, reclaim stale claims.
///
/// # Errors
///
/// Returns an [`EnrichError`] variant describing claim, fetch, wall, or store failure.
pub fn prepare_enrich_db(db: &SourceDb) -> Result<(), EnrichError> {
    db.purge_reactions()
        .map_err(|e| EnrichError::Store(e.to_string()))?;
    let queued = db
        .ensure_enrich_stub_queue()
        .map_err(|e| EnrichError::Store(e.to_string()))?;
    let stale_cut = (Local::now()
        - Duration::from_std(STALE_CLAIM).unwrap_or(Duration::minutes(45)))
    .to_rfc3339();
    let reclaimed = db
        .reclaim_stale_enrich_claims(&stale_cut)
        .map_err(|e| EnrichError::Store(e.to_string()))?;
    info!(queued, reclaimed, "enrich db prepared");
    Ok(())
}

/// How manual `/enrich` applied text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrichManualVia {
    Cache,
    Tor,
}

/// Outcome of manual `/enrich` on one `LinkedIn` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrichManualResult {
    Ok {
        source_id: i64,
        via: EnrichManualVia,
    },
    Failed {
        source_id: i64,
        after: String,
    },
    Wall {
        source_id: i64,
        after: String,
    },
    Skipped {
        source_id: i64,
        reason: &'static str,
    },
}

/// True for personal `LinkedIn` post/activity URLs accepted by `/enrich`.
#[must_use]
pub fn is_linkedin_enrich_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    if !lower.contains("linkedin.com") {
        return false;
    }
    if lower.contains("/in/") || lower.contains("lnkd.in") {
        return false;
    }
    lower.contains("/posts/")
        || lower.contains("/feed/update/urn:li:activity:")
        || lower.contains("/feed/update/")
}

/// Validates a URL for manual Tor enrich (slash `/enrich`).
///
/// # Errors
///
/// Returns an [`EnrichError`] variant describing claim, fetch, wall, or store failure.
pub fn validate_linkedin_enrich_url(url: &str) -> Result<(), EnrichError> {
    let t = url.trim();
    if !t.starts_with("http://") && !t.starts_with("https://") {
        return Err(EnrichError::Other(
            "URL must start with https://".to_string(),
        ));
    }
    if !is_linkedin_enrich_url(t) {
        return Err(EnrichError::Other(
            "LinkedIn enrich accepts post/activity URLs only (use /ingest for publisher pages)"
                .to_string(),
        ));
    }
    Ok(())
}

/// Manual Tor enrich for one `LinkedIn` post URL (Slack `/enrich`).
///
/// # Errors
///
/// Returns an [`EnrichError`] variant describing claim, fetch, wall, or store failure.
///
/// `SourceDb` / scrape cache use `rusqlite::Connection` (!Send); futures hold `&` across `.await`.
#[allow(clippy::future_not_send)]
pub async fn enrich_linkedin_url_at(
    db: &SourceDb,
    cache: &ScrapeCache,
    fetcher: &dyn TorPageFetcher,
    embed: &dyn EmbedClient,
    control_addr: Option<&str>,
    url: &str,
) -> Result<EnrichManualResult, EnrichError> {
    validate_linkedin_enrich_url(url)?;
    let source_id = db
        .upsert_manual_enrich_source(url)
        .map_err(|e| EnrichError::Store(e.to_string()))?;
    if db
        .get_source(source_id)
        .map_err(|e| EnrichError::Store(e.to_string()))?
        .is_some_and(|r| r.enrich_status == "skip")
    {
        return Ok(EnrichManualResult::Skipped {
            source_id,
            reason: "post_unavailable",
        });
    }
    let now = Local::now().to_rfc3339();
    let row = db
        .claim_enrich_by_id(source_id, &now)
        .map_err(|e| EnrichError::Store(e.to_string()))?;
    let claim_url = row.url.clone().unwrap_or_else(|| url.trim().to_string());
    let host_path = url_host_path(&claim_url);
    info!(
        id = row.id,
        activity = %row.activity,
        path = %host_path,
        "manual enrich claim"
    );
    let result = fetch_and_apply(db, cache, fetcher, embed, &row, &claim_url, control_addr).await;
    match result {
        Ok(src) => {
            let via = match src {
                ApplySource::Cache => EnrichManualVia::Cache,
                ApplySource::Tor => EnrichManualVia::Tor,
            };
            info!(
                id = row.id,
                path = %host_path,
                status = "ok",
                via = ?via,
                "manual enrich done"
            );
            Ok(EnrichManualResult::Ok {
                source_id: row.id,
                via,
            })
        }
        Err(e) => match classify_enrich_failure(db, row.id, &host_path, &e) {
            EnrichStep::Skipped { id, reason } => Ok(EnrichManualResult::Skipped {
                source_id: id,
                reason,
            }),
            EnrichStep::Wall { id, after } => Ok(EnrichManualResult::Wall {
                source_id: id,
                after,
            }),
            EnrichStep::Failed { id, after } => Ok(EnrichManualResult::Failed {
                source_id: id,
                after,
            }),
            EnrichStep::Idle | EnrichStep::Ok { .. } => Err(EnrichError::Other(
                "manual enrich: unexpected classify outcome".to_string(),
            )),
        },
    }
}

/// One claim → cache hit or Tor GET → in-place update + re-embed (or fail/backoff).
///
/// # Errors
///
/// Returns an [`EnrichError`] variant describing claim, fetch, wall, or store failure.
///
/// `SourceDb` / scrape cache use `rusqlite::Connection` (!Send); futures hold `&` across `.await`.
#[allow(clippy::future_not_send)]
pub async fn enrich_one(
    db: &SourceDb,
    cache: &ScrapeCache,
    fetcher: &dyn TorPageFetcher,
    embed: &dyn EmbedClient,
    control_addr: Option<&str>,
) -> Result<EnrichStep, EnrichError> {
    let now = Local::now().to_rfc3339();
    let Some(row) = db
        .claim_next_enrich(&now)
        .map_err(|e| EnrichError::Store(e.to_string()))?
    else {
        return Ok(EnrichStep::Idle);
    };
    let url = row.url.clone().unwrap_or_default();
    let host_path = url_host_path(&url);
    info!(
        id = row.id,
        activity = %row.activity,
        path = %host_path,
        "enrich claim"
    );

    // NEWNYM runs inside fetch_and_apply immediately before each Tor GET (not after).
    let result = fetch_and_apply(db, cache, fetcher, embed, &row, &url, control_addr).await;
    match result {
        Ok(src) => {
            info!(
                id = row.id,
                path = %host_path,
                status = "ok",
                via = ?src,
                "enrich done"
            );
            Ok(EnrichStep::Ok { id: row.id })
        }
        Err(e) => Ok(classify_enrich_failure(db, row.id, &host_path, &e)),
    }
}

fn classify_enrich_failure(db: &SourceDb, id: i64, host_path: &str, e: &EnrichError) -> EnrichStep {
    let msg = e.to_string();
    // Deleted activity: out of drip until manual requeue (HTML stays in scrape-cache).
    if msg.contains("post_unavailable") {
        let _ = db.skip_enrich(id);
        error!(
            id,
            path = %host_path,
            status = "skip",
            error = %e,
            "enrich parked (post unavailable)"
        );
        return EnrichStep::Skipped {
            id,
            reason: "post_unavailable",
        };
    }
    // True LinkedIn wall / rate limit.
    let wall = msg.contains("wall/") || msg.contains("429") || msg.contains("999");
    // Tor exit / hard HTTP: long backoff so we do not thrash every 20m; not a wall streak.
    let hard_http = msg.contains("HTTP 403")
        || msg.contains("HTTP 401")
        || msg.contains("HTTP 404")
        || msg.contains("HTTP 5");
    let backoff = if wall || hard_http {
        if hard_http && !wall {
            HARD_HTTP_BACKOFF
        } else {
            WALL_BACKOFF
        }
    } else {
        BASE_DELAY
    };
    let after =
        (Local::now() + Duration::from_std(backoff).unwrap_or(Duration::minutes(20))).to_rfc3339();
    let _ = db.fail_enrich(id, &after);
    if hard_http || wall {
        error!(
            id,
            path = %host_path,
            status = "failed",
            next_after = %after,
            error = %e,
            "enrich failed"
        );
    } else {
        warn!(
            id,
            path = %host_path,
            status = "failed",
            next_after = %after,
            error = %e,
            "enrich failed"
        );
    }
    if wall {
        EnrichStep::Wall { id, after }
    } else {
        EnrichStep::Failed { id, after }
    }
}

/// Where enrich text came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplySource {
    Cache,
    Tor,
}

/// Outcome of one loop iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrichStep {
    Idle,
    Ok {
        id: i64,
    },
    Failed {
        id: i64,
        after: String,
    },
    Wall {
        id: i64,
        after: String,
    },
    /// Parked out of drip (`skip`); e.g. deleted post. Refetch later by hand.
    Skipped {
        id: i64,
        reason: &'static str,
    },
}

/// `SourceDb` / scrape cache use `rusqlite::Connection` (!Send); futures hold `&` across `.await`.
#[allow(clippy::future_not_send)]
async fn fetch_and_apply(
    db: &SourceDb,
    cache: &ScrapeCache,
    fetcher: &dyn TorPageFetcher,
    embed: &dyn EmbedClient,
    row: &crate::sources::store::SourceRecord,
    url: &str,
    control_addr: Option<&str>,
) -> Result<ApplySource, EnrichError> {
    // Prefer re-extract from cached raw_html so extract upgrades apply without re-Tor.
    if let Some(hit) = cache
        .get(url)
        .map_err(|e| EnrichError::Store(e.to_string()))?
    {
        if !hit.raw_html.is_empty() {
            let extracted = extract_linkedin_public_post(&hit.raw_html);
            let page = ScrapePage {
                url: url.to_string(),
                fetched_at: hit.fetched_at.clone(),
                http_status: hit.http_status,
                raw_html: hit.raw_html.clone(),
                extracted_text: extracted.text.clone(),
                ok: extracted.ok,
            };
            cache
                .upsert(&page)
                .map_err(|e| EnrichError::Store(e.to_string()))?;
            if extracted.ok {
                info!(
                    path = %url_host_path(url),
                    reason = extracted.reason,
                    "scrape-cache hit (re-extract)"
                );
                apply_text(db, embed, row, &extracted.text).await?;
                return Ok(ApplySource::Cache);
            }
            // Dead post: park via classify (skip). Too thin: definitive from cache, soft fail.
            if extracted.reason == "post_unavailable" {
                return Err(EnrichError::Fetch(format!(
                    "extract failed ({})",
                    extracted.reason
                )));
            }
            if extracted.reason == "too_thin" {
                return Err(EnrichError::Fetch(format!(
                    "extract failed ({})",
                    extracted.reason
                )));
            }
            warn!(
                path = %url_host_path(url),
                reason = extracted.reason,
                "scrape-cache re-extract failed - will Tor fetch"
            );
        } else if hit.ok && non_empty_enrich_text(&hit.extracted_text) {
            info!(path = %url_host_path(url), "scrape-cache hit (text only)");
            apply_text(db, embed, row, &hit.extracted_text).await?;
            return Ok(ApplySource::Cache);
        }
    }

    if let Some(addr) = control_addr {
        match tor_newnym(addr) {
            Ok(()) => {
                info!(
                    settle_secs = NEWNYM_SETTLE.as_secs(),
                    "NEWNYM before Tor GET"
                );
                tokio::time::sleep(NEWNYM_SETTLE).await;
            }
            Err(e) => warn!(error = %e, "NEWNYM before fetch failed (continuing)"),
        }
    }

    let html = fetcher.fetch_html(url).await?;
    let extracted = extract_linkedin_public_post(&html);
    let page = ScrapePage {
        url: url.to_string(),
        fetched_at: Local::now().to_rfc3339(),
        http_status: Some(200),
        raw_html: html,
        extracted_text: extracted.text.clone(),
        ok: extracted.ok,
    };
    cache
        .upsert(&page)
        .map_err(|e| EnrichError::Store(e.to_string()))?;
    if !extracted.ok {
        let msg = if extracted.reason == "guest_chrome_no_body" {
            format!("wall/extract failed ({})", extracted.reason)
        } else {
            // post_unavailable / too_thin: soft fail (normal backoff, no wall streak).
            format!("extract failed ({})", extracted.reason)
        };
        return Err(EnrichError::Fetch(msg));
    }
    apply_text(db, embed, row, &extracted.text).await?;
    Ok(ApplySource::Tor)
}

/// `SourceDb` uses `rusqlite::Connection` (!Send); future holds `&` across embed `.await`.
#[allow(clippy::future_not_send)]
async fn apply_text(
    db: &SourceDb,
    embed: &dyn EmbedClient,
    row: &crate::sources::store::SourceRecord,
    text: &str,
) -> Result<(), EnrichError> {
    let subject = infer_subject(&row.title, text);
    let model = default_embed_model();
    let pieces = chunk_text(text, 800);
    db.delete_chunks_for_source(row.id)
        .map_err(|e| EnrichError::Store(e.to_string()))?;
    for piece in &pieces {
        let emb = embed
            .embed(&model, piece)
            .await
            .map_err(|e| EnrichError::Embed(e.to_string()))?;
        db.insert_chunk(row.id, &subject, piece, &emb)
            .map_err(|e| EnrichError::Store(e.to_string()))?;
    }
    let now = Local::now().to_rfc3339();
    db.complete_enrich(row.id, text, &subject, &now)
        .map_err(|e| EnrichError::Store(e.to_string()))?;
    Ok(())
}

/// Sleep duration: 20m + 0..=10m jitter.
#[must_use]
pub fn drip_delay() -> StdDuration {
    let jitter = {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        u64::try_from(nanos % (u128::from(JITTER_MAX_SECS) + 1)).unwrap_or(0)
    };
    BASE_DELAY + StdDuration::from_secs(jitter)
}

fn url_host_path(url: &str) -> String {
    url.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .chars()
        .take(120)
        .collect()
}

fn non_empty_enrich_text(s: &str) -> bool {
    !s.trim().is_empty()
}

/// Default probe URL (public activity; overridable via env).
#[must_use]
pub fn default_probe_url() -> String {
    std::env::var("ITCY_ENRICH_PROBE_URL").unwrap_or_else(|_| {
        "https://www.linkedin.com/feed/update/urn:li:activity:7349063659886534657".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::embed::MockEmbedClient;
    use crate::sources::store::InsertSource;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn stub_detection() {
        assert!(is_link_stub("Post\nhttps://li/1"));
        assert!(is_link_stub("Repost\nhttps://li/2"));
        assert!(is_link_stub("short"));
        assert!(!is_link_stub(
            "This is a long enough LinkedIn post body with real commentary about Rust async and tokio workers so enrichment leaves it alone forever."
        ));
    }

    #[test]
    fn transient_tor_fetch_detection() {
        assert!(is_transient_tor_fetch(&EnrichError::Fetch(
            "error sending request for url (https://www.linkedin.com/x)".into()
        )));
        assert!(is_transient_tor_fetch(&EnrichError::Fetch(
            "error decoding response body for url (https://www.linkedin.com/x)".into()
        )));
        assert!(is_transient_tor_fetch(&EnrichError::Fetch(
            "operation timed out".into()
        )));
        assert!(!is_transient_tor_fetch(&EnrichError::Fetch(
            "HTTP 403 Forbidden".into()
        )));
        assert!(!is_transient_tor_fetch(&EnrichError::Fetch(
            "wall HTTP 429 Too Many Requests".into()
        )));
        assert!(!is_transient_tor_fetch(&EnrichError::Fetch(
            "extract failed (post_unavailable)".into()
        )));
    }

    fn rich_html() -> String {
        format!(
            "<html><body>{}</body></html>",
            "Rich LinkedIn commentary about Rust and distributed systems. ".repeat(5)
        )
    }

    struct CountingFetcher {
        pages: HashMap<String, String>,
        hits: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl TorPageFetcher for CountingFetcher {
        async fn fetch_html(&self, url: &str) -> Result<String, EnrichError> {
            self.hits.fetch_add(1, Ordering::SeqCst);
            self.pages
                .get(url)
                .cloned()
                .ok_or_else(|| EnrichError::Fetch(format!("mock miss: {url}")))
        }
    }

    #[tokio::test]
    async fn ok_row_not_claimed_again() {
        let dir = TempDir::new().expect("temp");
        let db = SourceDb::open(dir.path().join("e.db")).expect("open");
        let cache = ScrapeCache::open(dir.path().join("c.db")).expect("cache");
        let sid = db
            .insert_source(&InsertSource {
                kind: "personal_feed",
                activity: "post",
                subject: "x",
                title: "stub",
                url: Some("https://www.linkedin.com/feed/update/urn:li:activity:1"),
                raw_text: "Post\nhttps://www.linkedin.com/feed/update/urn:li:activity:1",
                occurred_at: Some("2025-01-01T00:00:00"),
            })
            .expect("ins")
            .expect("id");
        db.ensure_enrich_stub_queue().expect("queue");
        let mut pages = HashMap::new();
        pages.insert(
            "https://www.linkedin.com/feed/update/urn:li:activity:1".into(),
            rich_html(),
        );
        let fetcher = MockTorFetcher { pages };
        let step = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("enrich");
        assert!(matches!(step, EnrichStep::Ok { id } if id == sid));
        let row = db.get_source(sid).expect("get").expect("row");
        assert_eq!(row.enrich_status, "ok");
        let step2 = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("second");
        assert_eq!(step2, EnrichStep::Idle);
        assert!(
            cache
                .get("https://www.linkedin.com/feed/update/urn:li:activity:1")
                .expect("cget")
                .expect("hit")
                .ok
        );
    }

    #[tokio::test]
    async fn cache_hit_skips_fetcher_after_sources_wipe() {
        let dir = TempDir::new().expect("temp");
        let db_path = dir.path().join("e.db");
        let cache = ScrapeCache::open(dir.path().join("c.db")).expect("cache");
        let url = "https://www.linkedin.com/feed/update/urn:li:activity:99";
        let db = SourceDb::open(&db_path).expect("open");
        let stub = format!("Repost\n{url}");
        db.insert_source(&InsertSource {
            kind: "personal_feed",
            activity: "repost",
            subject: "x",
            title: "Repost",
            url: Some(url),
            raw_text: &stub,
            occurred_at: Some("2025-06-01T00:00:00"),
        })
        .expect("ins")
        .expect("id");
        db.ensure_enrich_stub_queue().expect("q");
        let mut pages = HashMap::new();
        pages.insert(url.into(), rich_html());
        let hits = Arc::new(AtomicUsize::new(0));
        let fetcher = CountingFetcher {
            pages,
            hits: Arc::clone(&hits),
        };
        enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("first");
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // Wipe sources and re-stub the same URL: cache must prevent Tor re-fetch.
        db.clear_corpus().expect("wipe");
        let stub2 = format!("Repost\n{url}");
        db.insert_source(&InsertSource {
            kind: "personal_feed",
            activity: "repost",
            subject: "x",
            title: "Repost",
            url: Some(url),
            raw_text: &stub2,
            occurred_at: Some("2025-06-01T00:00:00"),
        })
        .expect("reins")
        .expect("id2");
        db.ensure_enrich_stub_queue().expect("q2");
        let step = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("cached");
        assert!(matches!(step, EnrichStep::Ok { .. }));
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "fetcher must not run on cache hit"
        );
    }

    #[test]
    fn stale_inflight_reclaimed() {
        let dir = TempDir::new().expect("temp");
        let db = SourceDb::open(dir.path().join("e.db")).expect("open");
        let sid = db
            .insert_source(&InsertSource {
                kind: "personal_feed",
                activity: "repost",
                subject: "x",
                title: "Repost",
                url: Some("https://li/r"),
                raw_text: "Repost\nhttps://li/r",
                occurred_at: Some("2025-06-01T00:00:00"),
            })
            .expect("ins")
            .expect("id");
        db.ensure_enrich_stub_queue().expect("q");
        let old = "2020-01-01T00:00:00+00:00";
        db.test_force_enrich_inflight(sid, old).expect("force");
        let n = db
            .reclaim_stale_enrich_claims("2024-01-01T00:00:00+00:00")
            .expect("reclaim");
        assert_eq!(n, 1);
        let row = db.get_source(sid).expect("g").expect("r");
        assert_eq!(row.enrich_status, "pending");
    }

    #[test]
    fn pid_lock_rejects_second() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("enrich.pid");
        let _a = PidLock::acquire(&path).expect("first");
        let err = PidLock::acquire(&path).expect_err("second");
        assert!(err.to_string().contains("already running"));
    }

    fn insert_stub(db: &SourceDb, activity: &str, url: &str) -> i64 {
        let title = if activity == "repost" {
            "Repost"
        } else {
            "Post"
        };
        let raw = format!("{title}\n{url}");
        db.insert_source(&InsertSource {
            kind: "personal_feed",
            activity,
            subject: "x",
            title,
            url: Some(url),
            raw_text: &raw,
            occurred_at: Some("2025-06-01T00:00:00"),
        })
        .expect("ins")
        .expect("id")
    }

    fn unavailable_html() -> String {
        r"
        <html><body>
        <h1>Agree & Join LinkedIn</h1>
        <p>This post is unavailable.</p>
        <p>Sign in to view more content.</p>
        </body></html>
        "
        .into()
    }

    fn guest_chrome_html() -> String {
        r"
        <html><body>
        <h1>Agree & Join LinkedIn</h1>
        <p>Sign in to view more content. Create your free account or sign in.</p>
        </body></html>
        "
        .into()
    }

    fn short_link_html() -> String {
        r#"
        <html><head>
        <meta property="og:title" content="X">
        </head><body>
        <h1>Gregory Roussac's Post</h1>
        </body></html>
        "#
        .into()
    }

    /// Fetcher that always returns a fixed `EnrichError::Fetch` message (no HTML).
    struct ErrFetcher {
        msg: String,
    }

    #[async_trait]
    impl TorPageFetcher for ErrFetcher {
        async fn fetch_html(&self, _url: &str) -> Result<String, EnrichError> {
            Err(EnrichError::Fetch(self.msg.clone()))
        }
    }

    fn assert_backoff_hours(after: &str, min_h: i64, max_h: i64) {
        let after_t = chrono::DateTime::parse_from_rfc3339(after).expect("after rfc3339");
        let now = Local::now();
        let delta = after_t.signed_duration_since(now);
        assert!(
            delta.num_hours() >= min_h && delta.num_hours() <= max_h,
            "expected {min_h}..={max_h}h backoff, got {delta:?} (after={after})"
        );
    }

    fn assert_backoff_minutes(after: &str, min_m: i64, max_m: i64) {
        let after_t = chrono::DateTime::parse_from_rfc3339(after).expect("after rfc3339");
        let now = Local::now();
        let delta = after_t.signed_duration_since(now);
        assert!(
            delta.num_minutes() >= min_m && delta.num_minutes() <= max_m,
            "expected {min_m}..={max_m}m backoff, got {delta:?} (after={after})"
        );
    }

    #[tokio::test]
    async fn unavailable_via_tor_parks_as_skip() {
        let dir = TempDir::new().expect("temp");
        let db = SourceDb::open(dir.path().join("e.db")).expect("open");
        let cache = ScrapeCache::open(dir.path().join("c.db")).expect("cache");
        let url = "https://www.linkedin.com/feed/update/urn:li:activity:unavail1";
        let sid = insert_stub(&db, "repost", url);
        db.ensure_enrich_stub_queue().expect("q");
        let mut pages = HashMap::new();
        pages.insert(url.into(), unavailable_html());
        let fetcher = MockTorFetcher { pages };
        let step = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("enrich");
        assert_eq!(
            step,
            EnrichStep::Skipped {
                id: sid,
                reason: "post_unavailable",
            }
        );
        let row = db.get_source(sid).expect("g").expect("r");
        assert_eq!(row.enrich_status, "skip");
        assert!(row.enrich_after.is_none());
        // Parked: not claimable again.
        let step2 = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("second");
        assert_eq!(step2, EnrichStep::Idle);
        let hit = cache.get(url).expect("c").expect("hit");
        assert!(!hit.ok);
        assert!(hit.raw_html.to_ascii_lowercase().contains("unavailable"));
    }

    #[tokio::test]
    async fn unavailable_from_cache_parks_without_tor() {
        let dir = TempDir::new().expect("temp");
        let db = SourceDb::open(dir.path().join("e.db")).expect("open");
        let cache = ScrapeCache::open(dir.path().join("c.db")).expect("cache");
        let url = "https://www.linkedin.com/feed/update/urn:li:activity:unavail2";
        let sid = insert_stub(&db, "repost", url);
        db.ensure_enrich_stub_queue().expect("q");
        cache
            .upsert(&ScrapePage {
                url: url.into(),
                fetched_at: Local::now().to_rfc3339(),
                http_status: Some(200),
                raw_html: unavailable_html(),
                extracted_text: String::new(),
                ok: false,
            })
            .expect("cache upsert");
        let hits = Arc::new(AtomicUsize::new(0));
        let fetcher = CountingFetcher {
            pages: HashMap::new(),
            hits: Arc::clone(&hits),
        };
        let step = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("enrich");
        assert_eq!(
            step,
            EnrichStep::Skipped {
                id: sid,
                reason: "post_unavailable",
            }
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "must not Tor on cache unavailable"
        );
        assert_eq!(
            db.get_source(sid).expect("g").expect("r").enrich_status,
            "skip"
        );
    }

    #[tokio::test]
    async fn http_403_failed_with_six_hour_backoff_not_wall() {
        let dir = TempDir::new().expect("temp");
        let db = SourceDb::open(dir.path().join("e.db")).expect("open");
        let cache = ScrapeCache::open(dir.path().join("c.db")).expect("cache");
        let url = "https://www.linkedin.com/feed/update/urn:li:activity:403";
        let sid = insert_stub(&db, "repost", url);
        db.ensure_enrich_stub_queue().expect("q");
        let fetcher = ErrFetcher {
            msg: "HTTP 403 Forbidden".into(),
        };
        let step = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("enrich");
        match step {
            EnrichStep::Failed { id, after } => {
                assert_eq!(id, sid);
                assert_backoff_hours(&after, 5, 7);
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        let row = db.get_source(sid).expect("g").expect("r");
        assert_eq!(row.enrich_status, "failed");
        // Still in future → not claimed.
        let step2 = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("second");
        assert_eq!(step2, EnrichStep::Idle);
        assert!(
            cache.get(url).expect("c").is_none(),
            "403 must not write HTML cache"
        );
    }

    #[tokio::test]
    async fn http_429_is_wall_with_six_hour_backoff() {
        let dir = TempDir::new().expect("temp");
        let db = SourceDb::open(dir.path().join("e.db")).expect("open");
        let cache = ScrapeCache::open(dir.path().join("c.db")).expect("cache");
        let url = "https://www.linkedin.com/feed/update/urn:li:activity:429";
        let sid = insert_stub(&db, "post", url);
        db.ensure_enrich_stub_queue().expect("q");
        let fetcher = ErrFetcher {
            msg: "wall HTTP 429 Too Many Requests".into(),
        };
        let step = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("enrich");
        match step {
            EnrichStep::Wall { id, after } => {
                assert_eq!(id, sid);
                assert_backoff_hours(&after, 5, 7);
            }
            other => panic!("expected Wall, got {other:?}"),
        }
        assert_eq!(
            db.get_source(sid).expect("g").expect("r").enrich_status,
            "failed"
        );
    }

    #[tokio::test]
    async fn guest_chrome_no_body_is_wall() {
        let dir = TempDir::new().expect("temp");
        let db = SourceDb::open(dir.path().join("e.db")).expect("open");
        let cache = ScrapeCache::open(dir.path().join("c.db")).expect("cache");
        let url = "https://www.linkedin.com/feed/update/urn:li:activity:chrome";
        let sid = insert_stub(&db, "post", url);
        db.ensure_enrich_stub_queue().expect("q");
        let mut pages = HashMap::new();
        pages.insert(url.into(), guest_chrome_html());
        let fetcher = MockTorFetcher { pages };
        let step = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("enrich");
        match step {
            EnrichStep::Wall { id, after } => {
                assert_eq!(id, sid);
                assert_backoff_hours(&after, 5, 7);
            }
            other => panic!("expected Wall, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn soft_too_thin_uses_base_delay_not_six_hours() {
        let dir = TempDir::new().expect("temp");
        let db = SourceDb::open(dir.path().join("e.db")).expect("open");
        let cache = ScrapeCache::open(dir.path().join("c.db")).expect("cache");
        let url = "https://www.linkedin.com/feed/update/urn:li:activity:thin";
        let sid = insert_stub(&db, "post", url);
        db.ensure_enrich_stub_queue().expect("q");
        // Card marker present but no usable title/body → too_thin (not wall).
        let thin = r#"
        <html><head>
        <script type="application/ld+json">{"@type":"SocialMediaPosting","text":""}</script>
        </head><body></body></html>
        "#;
        cache
            .upsert(&ScrapePage {
                url: url.into(),
                fetched_at: Local::now().to_rfc3339(),
                http_status: Some(200),
                raw_html: thin.into(),
                extracted_text: String::new(),
                ok: false,
            })
            .expect("cache");
        let fetcher = ErrFetcher {
            msg: "should not fetch".into(),
        };
        let step = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("enrich");
        match step {
            EnrichStep::Failed { id, after } => {
                assert_eq!(id, sid);
                assert_backoff_minutes(&after, 15, 35);
            }
            other => panic!("expected Failed soft, got {other:?}"),
        }
        assert_eq!(
            db.get_source(sid).expect("g").expect("r").enrich_status,
            "failed"
        );
    }

    #[tokio::test]
    async fn short_og_title_from_cache_enriches_ok() {
        let dir = TempDir::new().expect("temp");
        let db = SourceDb::open(dir.path().join("e.db")).expect("open");
        let cache = ScrapeCache::open(dir.path().join("c.db")).expect("cache");
        let url = "https://www.linkedin.com/feed/update/urn:li:activity:short";
        let sid = insert_stub(&db, "post", url);
        db.ensure_enrich_stub_queue().expect("q");
        cache
            .upsert(&ScrapePage {
                url: url.into(),
                fetched_at: Local::now().to_rfc3339(),
                http_status: Some(200),
                raw_html: short_link_html(),
                extracted_text: String::new(),
                ok: false,
            })
            .expect("cache");
        let hits = Arc::new(AtomicUsize::new(0));
        let fetcher = CountingFetcher {
            pages: HashMap::new(),
            hits: Arc::clone(&hits),
        };
        let step = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("enrich");
        assert!(matches!(step, EnrichStep::Ok { id } if id == sid));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        let row = db.get_source(sid).expect("g").expect("r");
        assert_eq!(row.enrich_status, "ok");
        assert!(row.raw_text.contains('X'), "{}", row.raw_text);
    }

    #[tokio::test]
    async fn skip_not_selected_by_claim() {
        let dir = TempDir::new().expect("temp");
        let db = SourceDb::open(dir.path().join("e.db")).expect("open");
        let cache = ScrapeCache::open(dir.path().join("c.db")).expect("cache");
        let url = "https://www.linkedin.com/feed/update/urn:li:activity:skipme";
        let sid = insert_stub(&db, "repost", url);
        db.ensure_enrich_stub_queue().expect("q");
        let mut pages = HashMap::new();
        pages.insert(url.into(), unavailable_html());
        let fetcher = MockTorFetcher { pages };
        enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("park");
        assert_eq!(
            db.get_source(sid).expect("g").expect("r").enrich_status,
            "skip"
        );
        let step = enrich_one(&db, &cache, &fetcher, &MockEmbedClient, None)
            .await
            .expect("claim");
        assert_eq!(step, EnrichStep::Idle);
        let counts = db.enrich_status_counts().expect("counts");
        assert_eq!(counts.skip, 1);
        assert_eq!(counts.pending, 0);
    }

    #[test]
    fn skip_enrich_requires_in_flight() {
        let dir = TempDir::new().expect("temp");
        let db = SourceDb::open(dir.path().join("e.db")).expect("open");
        let sid = insert_stub(&db, "repost", "https://li/skip-direct");
        db.ensure_enrich_stub_queue().expect("q");
        let err = db.skip_enrich(sid).expect_err("not in_flight");
        assert!(err.to_string().contains("not in_flight"));
        let now = Local::now().to_rfc3339();
        db.test_force_enrich_inflight(sid, &now).expect("force");
        db.skip_enrich(sid).expect("skip");
        assert_eq!(
            db.get_source(sid).expect("g").expect("r").enrich_status,
            "skip"
        );
    }

    #[tokio::test]
    async fn manual_enrich_linkedin_url_at_ok() {
        let dir = TempDir::new().expect("temp");
        let db = SourceDb::open(dir.path().join("e.db")).expect("open");
        let cache = ScrapeCache::open(dir.path().join("c.db")).expect("cache");
        let url = "https://www.linkedin.com/feed/update/urn:li:activity:manual1";
        let mut pages = HashMap::new();
        pages.insert(url.into(), rich_html());
        let fetcher = MockTorFetcher { pages };
        let result = enrich_linkedin_url_at(&db, &cache, &fetcher, &MockEmbedClient, None, url)
            .await
            .expect("manual enrich");
        match result {
            EnrichManualResult::Ok { source_id, via } => {
                assert!(source_id > 0);
                assert_eq!(via, EnrichManualVia::Tor);
                let row = db.get_source(source_id).expect("get").expect("row");
                assert_eq!(row.enrich_status, "ok");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn linkedin_enrich_url_validation() {
        assert!(is_linkedin_enrich_url(
            "https://www.linkedin.com/posts/gregoryroussac_something-activity-123"
        ));
        assert!(is_linkedin_enrich_url(
            "https://www.linkedin.com/feed/update/urn:li:activity:7349063659886534657"
        ));
        assert!(!is_linkedin_enrich_url(
            "https://www.linkedin.com/in/gregoryroussac/"
        ));
        assert!(!is_linkedin_enrich_url("https://example.com/a"));
        assert!(
            validate_linkedin_enrich_url("https://www.linkedin.com/posts/gregoryroussac_x").is_ok()
        );
        assert!(validate_linkedin_enrich_url("https://example.com/a").is_err());
    }

    #[test]
    fn hard_http_and_wall_backoff_constants_match() {
        assert_eq!(HARD_HTTP_BACKOFF, WALL_BACKOFF);
        assert_eq!(HARD_HTTP_BACKOFF, StdDuration::from_hours(6));
        assert_eq!(BASE_DELAY, StdDuration::from_mins(20));
    }

    #[test]
    fn read_enrich_side_signals_parses_streak_and_pid() {
        let dir = TempDir::new().expect("temp");
        let db = dir.path().join("runtime.db");
        std::fs::write(&db, b"").expect("touch db");
        std::fs::write(
            dir.path().join("enrich-wall-streak.txt"),
            "updated_at=t\nwall_streak=3\nlast_wall_source_id=42\n",
        )
        .expect("streak");
        std::fs::write(dir.path().join("enrich-linkedin-urls.pid"), "999999001\n").expect("pid");
        let side = read_enrich_side_signals(&db);
        assert_eq!(side.wall_streak, Some(3));
        assert_eq!(side.last_wall_source_id, Some(42));
        assert_eq!(side.enrich_pid, Some(999_999_001));
        assert!(!side.enrich_running);
    }
}

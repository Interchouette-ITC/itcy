// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! `LinkedIn` Member Data Portability (Snapshot + Changelog) → sources DB.
//!
//! Same ITC app credentials as Community Management (`.linkedin` /
//! `LINKEDIN_ACCESS_TOKEN`). Does not wait on CM approval to code or test.
//! Inbox / messaging resources are never ingested.

use crate::sources::embed::{default_embed_model, EmbedClient};
use crate::sources::export::{chunk_text, ExportItem};
use crate::sources::html::infer_subject;
use crate::sources::store::{InsertSource, SourceDb};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::debug;

/// LinkedIn-Version header required by DMA Snapshot/Changelog endpoints.
pub const DMA_API_VERSION: &str = "202312";

/// Snapshot domains ingested into the sources DB (excludes `INBOX`).
pub const CORPUS_SNAPSHOT_DOMAINS: &[&str] = &[
    "PROFILE",
    "POSITIONS",
    "EDUCATION",
    "SKILLS",
    "CERTIFICATIONS",
    "RECOMMENDATIONS",
    "MEMBER_SHARE_INFO",
    "ALL_COMMENTS",
    "ARTICLES",
    "ENDORSEMENTS",
];

/// Errors from portability fetch or store.
#[derive(Debug, Error)]
pub enum PortabilityError {
    #[error("portability http: {0}")]
    Http(String),
    #[error("portability parse: {0}")]
    Parse(String),
    #[error("portability embed: {0}")]
    Embed(String),
    #[error("portability store: {0}")]
    Store(String),
    #[error("portability: {0}")]
    Other(String),
}

/// Fetches raw Snapshot / Changelog JSON (mockable).
#[async_trait]
pub trait PortabilityClient: Send + Sync {
    /// One page of `memberSnapshotData` for a domain (`start` pagination).
    async fn fetch_snapshot_page(
        &self,
        domain: &str,
        start: u32,
        count: u32,
    ) -> Result<Value, PortabilityError>;

    /// One page of `memberChangeLogs` (`start_time` = epoch ms, or none).
    async fn fetch_changelog_page(
        &self,
        start_time_ms: Option<u64>,
        count: u32,
    ) -> Result<Value, PortabilityError>;
}

/// Live `LinkedIn` REST client (Bearer token).
pub struct HttpPortabilityClient {
    token: String,
    http: reqwest::Client,
}

impl HttpPortabilityClient {
    /// Builds a client from an access token.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl PortabilityClient for HttpPortabilityClient {
    async fn fetch_snapshot_page(
        &self,
        domain: &str,
        start: u32,
        count: u32,
    ) -> Result<Value, PortabilityError> {
        let url = format!(
            "https://api.linkedin.com/rest/memberSnapshotData?q=criteria&domain={domain}&start={start}&count={count}"
        );
        self.get_json(&url).await
    }

    async fn fetch_changelog_page(
        &self,
        start_time_ms: Option<u64>,
        count: u32,
    ) -> Result<Value, PortabilityError> {
        let mut url = format!(
            "https://api.linkedin.com/rest/memberChangeLogs?q=memberAndApplication&count={count}"
        );
        if let Some(t) = start_time_ms {
            let _ = write!(url, "&startTime={t}");
        }
        self.get_json(&url).await
    }
}

impl HttpPortabilityClient {
    async fn get_json(&self, url: &str) -> Result<Value, PortabilityError> {
        let res = self
            .http
            .get(url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Linkedin-Version", DMA_API_VERSION)
            .header("X-Restli-Protocol-Version", "2.0.0")
            .send()
            .await
            .map_err(|e| PortabilityError::Http(e.to_string()))?;
        let status = res.status();
        let text = res
            .text()
            .await
            .map_err(|e| PortabilityError::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(PortabilityError::Http(format!("{status}: {text}")));
        }
        serde_json::from_str(&text).map_err(|e| PortabilityError::Parse(e.to_string()))
    }
}

/// In-memory fixture client for tests.
pub struct MockPortabilityClient {
    pub snapshot_by_domain: BTreeMap<String, Value>,
    pub changelog: Value,
}

#[async_trait]
impl PortabilityClient for MockPortabilityClient {
    async fn fetch_snapshot_page(
        &self,
        domain: &str,
        start: u32,
        _count: u32,
    ) -> Result<Value, PortabilityError> {
        if start > 0 {
            return Ok(serde_json::json!({
                "elements": [],
                "paging": { "start": start, "count": 10, "total": 0, "links": [] }
            }));
        }
        Ok(self
            .snapshot_by_domain
            .get(domain)
            .cloned()
            .unwrap_or_else(|| {
                serde_json::json!({
                    "elements": [],
                    "paging": { "start": start, "count": 10, "total": 0, "links": [] }
                })
            }))
    }

    async fn fetch_changelog_page(
        &self,
        _start_time_ms: Option<u64>,
        _count: u32,
    ) -> Result<Value, PortabilityError> {
        Ok(self.changelog.clone())
    }
}

/// Maps a Snapshot domain name to corpus kind.
#[must_use]
pub fn kind_for_snapshot_domain(domain: &str) -> &'static str {
    match domain {
        "ALL_COMMENTS" => "comment",
        "MEMBER_SHARE_INFO" | "ARTICLES" => "personal_feed",
        _ => "voice",
    }
}

/// Fine activity label for MCP / weekly merge dedupe.
#[must_use]
pub fn activity_for_snapshot_domain(domain: &str) -> &'static str {
    match domain {
        "ALL_COMMENTS" => "comment",
        "MEMBER_SHARE_INFO" | "ARTICLES" => "post",
        "POSITIONS" => "position",
        "EDUCATION" => "education",
        "SKILLS" => "skill",
        "CERTIFICATIONS" => "certification",
        "RECOMMENDATIONS" | "ENDORSEMENTS" => "recommendation",
        _ => "profile",
    }
}

/// True when a changelog `resourceName` is private messaging (skip).
#[must_use]
pub fn is_inbox_resource(resource_name: &str) -> bool {
    let lower = resource_name.to_ascii_lowercase();
    lower.contains("message")
        || lower.contains("messaging")
        || lower == "inbox"
        || lower.contains("conversation")
}

/// Parses one Snapshot API page into export items (skips empty text).
///
/// # Errors
///
/// Returns a [`PortabilityError`] variant for DMA fetch, parse, or store failure.
pub fn items_from_snapshot_page(
    domain: &str,
    page: &Value,
) -> Result<Vec<ExportItem>, PortabilityError> {
    let kind = kind_for_snapshot_domain(domain);
    let mut items = Vec::new();
    let elements = page
        .get("elements")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for el in elements {
        let rows = el
            .get("snapshotData")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for row in rows {
            if let Some(item) = snapshot_row_to_item(domain, kind, &row) {
                items.push(item);
            }
        }
    }
    Ok(items)
}

fn snapshot_row_to_item(domain: &str, kind: &str, row: &Value) -> Option<ExportItem> {
    let map = row.as_object()?;
    let get = |keys: &[&str]| -> Option<String> {
        for k in keys {
            if let Some(v) = map.get(*k).and_then(|x| x.as_str()) {
                let t = v.trim();
                if !t.is_empty() && t != "-" {
                    return Some(t.to_string());
                }
            }
            // DMA sometimes uses spaced keys matching export CSV headers.
            for (key, val) in map {
                if key.eq_ignore_ascii_case(k) {
                    if let Some(s) = val.as_str() {
                        let t = s.trim();
                        if !t.is_empty() && t != "-" {
                            return Some(t.to_string());
                        }
                    }
                }
            }
        }
        None
    };
    let text = get(&[
        "ShareCommentary",
        "Commentary",
        "Message",
        "Text",
        "Content",
        "Comments",
        "PostText",
        "Description",
        "Summary",
        "Headline",
        "Notes",
        "Review Text",
        "Media Description",
    ])?;
    let title = get(&[
        "Title",
        "Subject",
        "Company Name",
        "Name",
        "School Name",
        "Job Title",
    ])
    .unwrap_or_else(|| text.chars().take(60).collect());
    let url = get(&[
        "ShareLink",
        "SharedUrl",
        "Link",
        "Url",
        "Permalink",
        "Media Link",
    ]);
    let occurred_at = get(&["Date", "Date/Time", "Created At", "Creation Date"])
        .map(|s| crate::sources::export::normalize_linkedin_datetime(&s));
    let activity = activity_for_snapshot_domain(domain);
    Some(ExportItem {
        kind: kind.into(),
        activity: activity.into(),
        title,
        url,
        text,
        occurred_at,
    })
}

/// Parses Changelog page elements into export items (skips inbox).
///
/// # Errors
///
/// Returns a [`PortabilityError`] variant for DMA fetch, parse, or store failure.
pub fn items_from_changelog_page(page: &Value) -> Result<Vec<ExportItem>, PortabilityError> {
    let mut items = Vec::new();
    let elements = page
        .get("elements")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for el in elements {
        let resource = el
            .get("resourceName")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if is_inbox_resource(resource) {
            continue;
        }
        // Reactions / likes are out of inspiration scope.
        let lower = resource.to_ascii_lowercase();
        if (lower.contains("like") || lower.contains("reaction")) && !lower.contains("comment") {
            continue;
        }
        if let Some(item) = changelog_element_to_item(&el) {
            items.push(item);
        }
    }
    Ok(items)
}

fn changelog_element_to_item(el: &Value) -> Option<ExportItem> {
    let resource = el
        .get("resourceName")
        .and_then(|v| v.as_str())
        .unwrap_or("activity");
    let kind = if resource.to_ascii_lowercase().contains("comment") {
        "comment"
    } else if resource.to_ascii_lowercase().contains("like")
        || resource.to_ascii_lowercase().contains("reaction")
        || resource.to_ascii_lowercase().contains("share")
        || resource.to_ascii_lowercase().contains("ugc")
        || resource.to_ascii_lowercase().contains("post")
    {
        "personal_feed"
    } else {
        "voice"
    };
    let activity_label = if resource.to_ascii_lowercase().contains("comment") {
        "comment"
    } else if resource.to_ascii_lowercase().contains("like")
        || resource.to_ascii_lowercase().contains("reaction")
    {
        "reaction"
    } else if resource.to_ascii_lowercase().contains("repost") {
        "repost"
    } else if resource.to_ascii_lowercase().contains("share")
        || resource.to_ascii_lowercase().contains("ugc")
        || resource.to_ascii_lowercase().contains("post")
    {
        "post"
    } else {
        "profile"
    };
    let activity = el
        .get("processedActivity")
        .or_else(|| el.get("activity"))
        .cloned()
        .unwrap_or(Value::Null);
    if activity
        .get("message")
        .and_then(|m| m.as_str())
        .is_some_and(|m| m.contains("Unable to process"))
    {
        return None;
    }
    let text = extract_activity_text(&activity)?;
    let title: String = text.chars().take(60).collect();
    let url = activity
        .get("permalink")
        .or_else(|| activity.get("url"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let occurred_at = el
        .get("capturedAt")
        .or_else(|| el.get("occurredAt"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(crate::sources::export::normalize_linkedin_datetime);
    Some(ExportItem {
        kind: kind.into(),
        activity: activity_label.into(),
        title,
        url,
        text,
        occurred_at,
    })
}

fn extract_activity_text(activity: &Value) -> Option<String> {
    if let Some(s) = activity.as_str() {
        let t = s.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let obj = activity.as_object()?;
    for key in [
        "commentary",
        "ShareCommentary",
        "message",
        "text",
        "comment",
        "commentaryText",
    ] {
        if let Some(v) = obj.get(key) {
            if let Some(s) = v.as_str() {
                let t = s.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
            if let Some(inner) = v.get("text").and_then(|x| x.as_str()) {
                let t = inner.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
            if let Some(inner) = v
                .pointer("/content/string")
                .and_then(|x| x.as_str())
                .or_else(|| {
                    v.pointer("/content/content/string")
                        .and_then(|x| x.as_str())
                })
            {
                let t = inner.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    if let Some(content) = obj.get("content") {
        if let Some(s) = content
            .pointer("/content/string")
            .or_else(|| content.get("fallback"))
            .and_then(|x| x.as_str())
        {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Fetches all configured Snapshot domains (first page each) + one Changelog page; stores items.
///
/// # Errors
///
/// Returns a [`PortabilityError`] variant for DMA fetch, parse, or store failure.
pub async fn import_portability_corpus(
    client: &dyn PortabilityClient,
    db_path: &Path,
    embed: &dyn EmbedClient,
) -> Result<usize, PortabilityError> {
    let mut items = Vec::new();
    for domain in CORPUS_SNAPSHOT_DOMAINS {
        match client.fetch_snapshot_page(domain, 0, 10).await {
            Ok(page) => items.extend(items_from_snapshot_page(domain, &page)?),
            Err(e) => {
                // Empty / unsupported domains are normal; keep going.
                debug!(domain, error = %e, "sources: portability snapshot skipped");
            }
        }
    }
    match client.fetch_changelog_page(None, 10).await {
        Ok(page) => items.extend(items_from_changelog_page(&page)?),
        Err(e) => debug!(error = %e, "sources: portability changelog skipped"),
    }
    store_export_items(&items, db_path, embed).await
}

async fn store_export_items(
    items: &[ExportItem],
    db_path: &Path,
    embed: &dyn EmbedClient,
) -> Result<usize, PortabilityError> {
    struct Prepared {
        item: ExportItem,
        subject: String,
        chunks: Vec<(String, Vec<f32>)>,
    }
    let model = default_embed_model();
    let db = SourceDb::open(db_path).map_err(|e| PortabilityError::Store(e.to_string()))?;
    let mut prepared: Vec<Prepared> = Vec::new();
    for item in items {
        if item.text.trim().is_empty() {
            continue;
        }
        if db
            .source_exists(
                &item.activity,
                item.url.as_deref(),
                item.occurred_at.as_deref(),
                &item.title,
            )
            .map_err(|e| PortabilityError::Store(e.to_string()))?
        {
            continue;
        }
        let subject = infer_subject(&item.title, &item.text);
        let mut chunks = Vec::new();
        for chunk in chunk_text(&item.text, 800) {
            let embedding = embed
                .embed(&model, &chunk)
                .await
                .map_err(|e| PortabilityError::Embed(e.to_string()))?;
            chunks.push((chunk, embedding));
        }
        prepared.push(Prepared {
            item: item.clone(),
            subject,
            chunks,
        });
    }
    let stored = db
        .with_transaction(|conn| {
            let mut stored = 0usize;
            for row in &prepared {
                let Some(source_id) = SourceDb::insert_source_on(
                    conn,
                    &InsertSource {
                        kind: &row.item.kind,
                        activity: &row.item.activity,
                        subject: &row.subject,
                        title: &row.item.title,
                        url: row.item.url.as_deref(),
                        raw_text: &row.item.text,
                        occurred_at: row.item.occurred_at.as_deref(),
                    },
                )?
                else {
                    continue;
                };
                for (chunk, embedding) in &row.chunks {
                    SourceDb::insert_chunk_on(conn, source_id, &row.subject, chunk, embedding)?;
                }
                stored += 1;
            }
            Ok(stored)
        })
        .map_err(|e| PortabilityError::Store(e.to_string()))?;
    Ok(stored)
}

/// Resolves `LinkedIn` access token from env or `.linkedin` file.
#[must_use]
pub fn resolve_linkedin_access_token() -> Option<String> {
    if let Ok(t) = std::env::var("LINKEDIN_ACCESS_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    for candidate in linkedin_cred_candidates() {
        if let Some(t) = read_token_from_dotenv_file(&candidate) {
            return Some(t);
        }
    }
    None
}

fn linkedin_cred_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(".linkedin"));
        out.push(cwd.join("../.linkedin"));
    }
    out.push(crate::paths::product_join(".linkedin"));
    out
}

fn read_token_from_dotenv_file(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("LINKEDIN_ACCESS_TOKEN=") {
            let v = rest.trim().trim_matches('"').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::embed::MockEmbedClient;
    use tempfile::TempDir;

    fn profile_fixture() -> Value {
        serde_json::json!({
            "elements": [{
                "snapshotDomain": "PROFILE",
                "snapshotData": [{
                    "First Name": "Greg",
                    "Last Name": "Test",
                    "Headline": "Senior Rust Engineer",
                    "Summary": "Builds SDKs and distributed systems in Rust."
                }]
            }],
            "paging": { "start": 0, "count": 10, "total": 1, "links": [] }
        })
    }

    fn shares_fixture() -> Value {
        serde_json::json!({
            "elements": [{
                "snapshotDomain": "MEMBER_SHARE_INFO",
                "snapshotData": [{
                    "ShareCommentary": "Excited about Rust async and tokio",
                    "ShareLink": "https://www.linkedin.com/feed/update/urn:li:activity:1"
                }]
            }],
            "paging": { "start": 0, "count": 10, "total": 1, "links": [] }
        })
    }

    fn changelog_fixture() -> Value {
        serde_json::json!({
            "elements": [
                {
                    "resourceName": "ugcPosts",
                    "method": "CREATE",
                    "processedActivity": {
                        "commentary": "Shipped a TDD-first LinkedIn corpus importer"
                    },
                    "processedAt": 1
                },
                {
                    "resourceName": "messages",
                    "method": "CREATE",
                    "processedActivity": {
                        "content": { "fallback": "SECRET DM skip me" }
                    },
                    "processedAt": 2
                }
            ]
        })
    }

    #[test]
    fn snapshot_profile_and_shares_parse() {
        let items = items_from_snapshot_page("PROFILE", &profile_fixture()).unwrap();
        assert!(items.iter().any(|i| i.text.contains("SDKs")));
        let shares = items_from_snapshot_page("MEMBER_SHARE_INFO", &shares_fixture()).unwrap();
        assert!(shares.iter().any(|i| i.kind == "personal_feed"));
        assert!(shares.iter().any(|i| i.activity == "post"));
        assert!(!shares.iter().any(|i| i.kind == "voice"));
    }

    #[test]
    fn changelog_skips_messages() {
        let items = items_from_changelog_page(&changelog_fixture()).unwrap();
        assert!(items.iter().any(|i| i.text.contains("TDD-first")));
        assert!(items.iter().all(|i| !i.text.contains("SECRET DM")));
    }

    #[tokio::test]
    async fn import_mock_corpus_into_db() {
        let mut snapshots = BTreeMap::new();
        snapshots.insert("PROFILE".into(), profile_fixture());
        snapshots.insert("MEMBER_SHARE_INFO".into(), shares_fixture());
        let client = MockPortabilityClient {
            snapshot_by_domain: snapshots,
            changelog: changelog_fixture(),
        };
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("rag.db");
        let n = import_portability_corpus(&client, &db, &MockEmbedClient)
            .await
            .unwrap();
        assert!(n >= 3, "got {n}");
        let store = SourceDb::open(&db).unwrap();
        let chunks = store.get_chunk_candidates("rust", 40).unwrap();
        assert!(chunks
            .iter()
            .any(|c| c.text.to_ascii_lowercase().contains("rust")));
        assert!(chunks.iter().all(|c| !c.text.contains("SECRET DM")));
    }

    #[test]
    fn inbox_resource_detection() {
        assert!(is_inbox_resource("messages"));
        assert!(is_inbox_resource("messagingThreads"));
        assert!(!is_inbox_resource("ugcPosts"));
    }
}

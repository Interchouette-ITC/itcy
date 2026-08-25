// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Company-page `LinkedIn` publisher (playground and production modes).
//!
//! Default mode is **playground** (no network). Production ships via local MCP.
//! The REST Community Management client remains in-tree unused.
//!
//! Mode is **not** a one-way latch: env, `config.toml`, and per-call overrides
//! can flip playground ↔ production without a permanent lock.

mod audit;
mod live;
mod mcp;
mod mcp_status;
mod mock;
mod ship;
mod x;

pub use audit::{PublishAuditError, PublishAuditRow, PublishAuditStore, PublishAuditWrite};
pub use live::ProductionLinkedInPublisher;
pub use mcp::{
    activity_post_urn, linkedin_text_for_api, parent_comment_urn, LinkedInMcpClient,
    McpLinkedInPublisher,
};
pub use mcp_status::{
    log_linkedin_mcp_status, probe_linkedin_mcp, run_linkedin_mcp_watch_loop, LinkedInMcpStatus,
};
pub use mock::PlaygroundPublisher;
pub use ship::{draft_id_from_body, ship_company_post, ShipOptions};
pub use x::{
    resolve_x_publish_mode, ship_x_post, tweet_text_for_api, tweet_texts_for_api, XPublishRequest,
};

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

/// Default Interchouette ITC `LinkedIn` organization id (company page).
pub const DEFAULT_ORGANIZATION_ID: &str = "91634202";

/// LinkedIn-Version header for Community Management Posts API.
pub const CM_API_VERSION: &str = "202405";

/// How `ITCy` ships after BAT merge (playground = soft; production = real).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishMode {
    /// Soft ship: synthetic URL / URN; publications stay on the fork.
    Playground,
    /// Real ship (company-page CM API; X via Brave until paid API).
    Production,
}

impl PublishMode {
    /// Parses `playground` / `production`. Legacy aliases: `mock`, `live`.
    ///
    /// # Errors
    ///
    /// Returns [`PublishError::Config`] when the mode string is unknown.
    pub fn parse(raw: &str) -> Result<Self, PublishError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "playground" | "mock" => Ok(Self::Playground),
            "production" | "live" => Ok(Self::Production),
            other => Err(PublishError::Config(format!(
                "unknown publish mode `{other}` (want playground|production)"
            ))),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Playground => "playground",
            Self::Production => "production",
        }
    }
}

/// Input for one company-page post.
#[derive(Debug, Clone)]
pub struct PublishRequest {
    /// Operator draft id (`DRAFT-…`), when known.
    pub draft_id: Option<String>,
    /// Publications PR that just merged, when known.
    pub pubs_pr_number: Option<u64>,
    /// Post body (`LinkedIn` commentary / text).
    pub body: String,
}

/// Outcome of a publish attempt (playground or production).
#[derive(Debug, Clone)]
pub struct PublishResult {
    pub mode: PublishMode,
    /// `LinkedIn` post/share URN (or X status id for tweet audit rows).
    pub linkedin_urn: Option<String>,
    /// Public URL when known.
    pub linkedin_url: Option<String>,
    /// Short operator-facing detail for logs / Slack.
    pub detail: String,
}

impl PublishResult {
    /// Slack ship-notice body: public URL when known (same shape as X), else `detail`.
    #[must_use]
    pub fn ship_notice_text(&self) -> &str {
        self.linkedin_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .unwrap_or(self.detail.as_str())
    }
}

/// Errors from config, credentials, or the publish path.
#[derive(Debug, Error)]
pub enum PublishError {
    #[error("publish config: {0}")]
    Config(String),
    #[error("publish credentials: {0}")]
    Credentials(String),
    #[error("publish http: {0}")]
    Http(String),
    #[error("publish: {0}")]
    Other(String),
}

/// Ships a company-page post.
#[async_trait]
pub trait Publisher: Send + Sync {
    fn mode(&self) -> PublishMode;

    async fn publish_company_post(
        &self,
        request: &PublishRequest,
    ) -> Result<PublishResult, PublishError>;
}

/// Resolves mode: env `ITCY_LINKEDIN_PUBLISH_MODE` wins over config string.
///
/// # Errors
///
/// Returns a [`PublishError`] variant for mode, token, or `LinkedIn` publish failure.
pub fn resolve_publish_mode(config_mode: &str) -> Result<PublishMode, PublishError> {
    if let Ok(raw) = std::env::var("ITCY_LINKEDIN_PUBLISH_MODE") {
        let raw = raw.trim();
        if !raw.is_empty() {
            return PublishMode::parse(raw);
        }
    }
    PublishMode::parse(config_mode)
}

/// Per-ship resolution so playground/production stays agile (no forever latch).
///
/// Order: `ITCY_LINKEDIN_PUBLISH_MODE` env → `[linkedin].publish_mode` re-read from
/// `config.toml` on disk → `fallback` (boot config snapshot).
///
/// # Errors
///
/// Returns a [`PublishError`] variant for mode, token, or `LinkedIn` publish failure.
pub fn resolve_publish_mode_agile(fallback: &str) -> Result<PublishMode, PublishError> {
    if let Ok(raw) = std::env::var("ITCY_LINKEDIN_PUBLISH_MODE") {
        let raw = raw.trim();
        if !raw.is_empty() {
            return PublishMode::parse(raw);
        }
    }
    if let Some(from_disk) = read_publish_mode_from_config_toml() {
        return PublishMode::parse(&from_disk);
    }
    PublishMode::parse(fallback)
}

fn read_publish_mode_from_config_toml() -> Option<String> {
    read_section_publish_mode_from_config_toml("linkedin")
}

pub(crate) fn read_section_publish_mode_from_config_toml(section: &str) -> Option<String> {
    for path in ship::config_toml_candidates() {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(mode) = parse_section_publish_mode_toml(&raw, section) {
            return Some(mode);
        }
    }
    None
}

fn parse_section_publish_mode_toml(raw: &str, section: &str) -> Option<String> {
    let header = format!("[{section}]");
    let mut in_section = false;
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_section = line == header;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(rest) = line.strip_prefix("publish_mode") {
            let rest = rest.trim().trim_start_matches('=').trim();
            let v = rest.trim_matches('"').trim_matches('\'').trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Builds the publisher for the resolved mode.
///
/// Production fails at build time if token or org id is missing (no silent playground).
///
/// # Errors
///
/// Returns a [`PublishError`] variant for mode, token, or `LinkedIn` publish failure.
pub fn build_publisher(mode: PublishMode) -> Result<Arc<dyn Publisher>, PublishError> {
    match mode {
        PublishMode::Playground => Ok(Arc::new(PlaygroundPublisher)),
        PublishMode::Production => Ok(Arc::new(McpLinkedInPublisher::new())),
    }
}

/// Organization id from env / `.linkedin`, else default ITC page id.
#[must_use]
pub fn resolve_linkedin_organization_id() -> String {
    if let Ok(id) = std::env::var("LINKEDIN_ORGANIZATION_ID") {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return id;
        }
    }
    for candidate in linkedin_cred_candidates() {
        if let Some(id) = read_key_from_dotenv_file(&candidate, "LINKEDIN_ORGANIZATION_ID") {
            return id;
        }
    }
    DEFAULT_ORGANIZATION_ID.to_string()
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

fn read_key_from_dotenv_file(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let prefix = format!("{key}=");
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
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

    #[test]
    fn parse_mode_playground_production() {
        assert_eq!(
            PublishMode::parse("playground").unwrap(),
            PublishMode::Playground
        );
        assert_eq!(
            PublishMode::parse("PRODUCTION").unwrap(),
            PublishMode::Production
        );
        assert_eq!(PublishMode::parse("mock").unwrap(), PublishMode::Playground);
        assert_eq!(PublishMode::parse("live").unwrap(), PublishMode::Production);
        assert!(PublishMode::parse("dry-run").is_err());
    }

    #[test]
    fn ship_notice_prefers_public_url() {
        let with_url = PublishResult {
            mode: PublishMode::Production,
            linkedin_urn: Some("urn:li:share:1".into()),
            linkedin_url: Some("https://www.linkedin.com/feed/update/urn:li:share:1".into()),
            detail: "mcp ship ok Published text post".into(),
        };
        assert_eq!(
            with_url.ship_notice_text(),
            "https://www.linkedin.com/feed/update/urn:li:share:1"
        );
        let no_url = PublishResult {
            mode: PublishMode::Playground,
            linkedin_urn: None,
            linkedin_url: None,
            detail: "playground ship ok".into(),
        };
        assert_eq!(no_url.ship_notice_text(), "playground ship ok");
    }

    #[tokio::test]
    async fn playground_publisher_returns_synthetic_urn() {
        let pubr = PlaygroundPublisher;
        let result = pubr
            .publish_company_post(&PublishRequest {
                draft_id: Some("DRAFT-20260728-000022".into()),
                pubs_pr_number: Some(3),
                body: "Hello from playground".into(),
            })
            .await
            .expect("playground ok");
        assert_eq!(result.mode, PublishMode::Playground);
        let urn = result.linkedin_urn.expect("urn");
        assert!(urn.contains("DRAFT-20260728-000022"));
        assert!(result.linkedin_url.is_some());
        assert!(result.detail.contains("playground"));
    }

    #[test]
    fn production_without_token_refuses_at_build() {
        // Ensure env token cannot sneak in from the agent shell.
        let _guard = EnvGuard::unset("LINKEDIN_ACCESS_TOKEN");
        // Point creds at a missing path by using a temp cwd without .linkedin.
        let tmp = tempfile::TempDir::new().unwrap();
        let _cwd = EnvGuard::set_cwd(tmp.path());
        // Cred discovery may still find a real .linkedin via product_root; force None.
        let err = ProductionLinkedInPublisher::try_from_parts(None, Some("91634202".into()))
            .err()
            .expect("must refuse");
        let msg = err.to_string();
        assert!(
            msg.contains("LINKEDIN_ACCESS_TOKEN") || msg.contains("token"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn production_without_org_refuses_at_build() {
        let err = ProductionLinkedInPublisher::try_from_parts(
            Some("fake-token".into()),
            Some(String::new()),
        )
        .err()
        .expect("must refuse");
        assert!(err.to_string().contains("ORGANIZATION"));
    }

    #[test]
    fn parse_linkedin_publish_mode_toml_section() {
        let raw = r#"
[server]
bind = "127.0.0.1:4700"

[linkedin]
publish_mode = "production"
"#;
        assert_eq!(
            parse_section_publish_mode_toml(raw, "linkedin").as_deref(),
            Some("production")
        );
        assert_eq!(
            parse_section_publish_mode_toml(
                "[linkedin]\npublish_mode = \"playground\"\n[x]\npublish_mode = \"production\"\n",
                "x"
            )
            .as_deref(),
            Some("production")
        );
    }

    /// RAII env / cwd helpers for tests.
    struct EnvGuard {
        key: Option<&'static str>,
        prev: Option<String>,
        prev_cwd: Option<PathBuf>,
    }

    impl EnvGuard {
        fn unset(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self {
                key: Some(key),
                prev,
                prev_cwd: None,
            }
        }

        fn set_cwd(path: &Path) -> Self {
            let prev_cwd = std::env::current_dir().ok();
            std::env::set_current_dir(path).unwrap();
            Self {
                key: None,
                prev: None,
                prev_cwd,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(key) = self.key {
                match &self.prev {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
            if let Some(cwd) = &self.prev_cwd {
                let _ = std::env::set_current_dir(cwd);
            }
        }
    }
}

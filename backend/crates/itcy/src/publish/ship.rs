// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Ship orchestration after BAT merge (mode resolved per call; mock/live switchable).

use super::{
    build_publisher, linkedin_text_for_api, resolve_publish_mode_agile, PublishAuditStore,
    PublishAuditWrite, PublishError, PublishMode, PublishRequest, PublishResult,
};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Optional overrides for e2e / operator inject (never freezes process-wide mode).
#[derive(Debug, Clone, Default)]
pub struct ShipOptions {
    /// When set, forces this mode for one ship only (e2e mock without touching live).
    pub mode_override: Option<PublishMode>,
}

/// Runs publisher + audit for one post body after BAT merge.
///
/// Mode is resolved **per call** (override → env → config.toml on disk → fallback).
/// Publish failure is returned; callers must not un-merge.
///
/// # Errors
///
/// Returns a [`PublishError`] variant for mode, token, or `LinkedIn` publish failure.
pub async fn ship_company_post(
    state_db_path: impl AsRef<Path>,
    mode_fallback: &str,
    mut request: PublishRequest,
    options: ShipOptions,
) -> Result<PublishResult, PublishError> {
    let mode = match options.mode_override {
        Some(m) => m,
        None => resolve_publish_mode_agile(mode_fallback)?,
    };
    request.body = linkedin_text_for_api(&request.body);
    info!(
        mode = mode.as_str(),
        draft_id = request.draft_id.as_deref().unwrap_or(""),
        pubs_pr = request.pubs_pr_number.unwrap_or(0),
        "publish: ship starting"
    );
    let publisher = build_publisher(mode)?;
    let audit = PublishAuditStore::open(state_db_path.as_ref())
        .map_err(|e| PublishError::Other(e.to_string()))?;

    match publisher.publish_company_post(&request).await {
        Ok(result) => {
            if let Err(e) = audit.insert(&PublishAuditWrite::from_ok(&request, &result)) {
                warn!(error = %e, "publish: audit insert failed after ok ship");
            }
            info!(detail = %result.detail, "publish: ship ok");
            Ok(result)
        }
        Err(err) => {
            if let Err(e) = audit.insert(&PublishAuditWrite::from_err(&request, mode, &err)) {
                warn!(error = %e, "publish: audit insert failed after ship error");
            }
            Err(err)
        }
    }
}

/// Extracts `POST-…` / `DRAFT-…` / `TWEET-…` / `XPOST-…` from a body.md (first matching line).
#[must_use]
pub fn draft_id_from_body(body: &str) -> Option<String> {
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Post ID:") {
            let id = rest.trim();
            if id.starts_with("POST-") {
                return Some(id.to_string());
            }
        }
        if let Some(rest) = t.strip_prefix("Draft ID:") {
            let id = rest.trim();
            if id.starts_with("DRAFT-") {
                return Some(id.to_string());
            }
        }
        if let Some(rest) = t.strip_prefix("XPOST ID:") {
            let id = rest.trim();
            if id.starts_with("XPOST-") {
                return Some(id.to_string());
            }
        }
        if let Some(rest) = t.strip_prefix("Tweet ID:") {
            let id = rest.trim();
            if id.starts_with("TWEET-") {
                return Some(id.to_string());
            }
        }
        if t.starts_with("POST-")
            || t.starts_with("DRAFT-")
            || t.starts_with("TWEET-")
            || t.starts_with("XPOST-")
        {
            return Some(t.to_string());
        }
    }
    None
}

/// Candidate config.toml paths for agile mode re-read (no process restart required).
pub fn config_toml_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("ITCY_CONFIG") {
        let p = p.trim();
        if !p.is_empty() {
            out.push(PathBuf::from(p));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join("config.toml"));
        out.push(cwd.join("../backend/config.toml"));
    }
    out.push(crate::paths::product_join("backend/config.toml"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn draft_id_from_body_line() {
        let body = "Draft ID: DRAFT-20260728-000022\n\nHello";
        assert_eq!(
            draft_id_from_body(body).as_deref(),
            Some("DRAFT-20260728-000022")
        );
        let post = "Post ID: POST-20260728-000022\n\nHello";
        assert_eq!(
            draft_id_from_body(post).as_deref(),
            Some("POST-20260728-000022")
        );
    }

    #[tokio::test]
    async fn ship_mock_writes_audit() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("s.db");
        let result = ship_company_post(
            &db,
            "production", // fallback ignored when override is playground
            PublishRequest {
                draft_id: Some("DRAFT-TEST-000001".into()),
                pubs_pr_number: Some(9),
                body: "Draft ID: DRAFT-TEST-000001\n\nhi".into(),
            },
            ShipOptions {
                mode_override: Some(PublishMode::Playground),
            },
        )
        .await
        .expect("ship");
        assert_eq!(result.mode, PublishMode::Playground);
        let store = PublishAuditStore::open(&db).unwrap();
        let row = store.get(1).unwrap().expect("row");
        assert_eq!(row.status, "ok");
        assert_eq!(row.mode, "playground");
        assert!(
            !row.body_preview.contains("Draft ID:"),
            "ship must strip id header: {}",
            row.body_preview
        );
        assert!(row.body_preview.contains("hi"), "{}", row.body_preview);
    }
}

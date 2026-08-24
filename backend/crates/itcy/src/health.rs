// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! HTTP health + runtime status + GitHub webhook + local E2E inject routes.

use crate::hooks::{hooks_github_wake, BatWakeSnapshot, GithubDeliverySnapshot, GithubHookState};
use crate::llm::router::TaskKind;
use crate::llm::FailoverRouter;
use crate::publish::{probe_linkedin_mcp, LinkedInMcpStatus};
use crate::slack::SlackRuntime;
use crate::sources::{
    probe_tor_listen, probe_twitter_vault, read_enrich_side_signals, EnrichStatusCounts, SourceDb,
    TorListenStatus, TwitterVaultStatus,
};
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Shared HTTP state (LLM router + GitHub webhook wake + optional operator for E2E).
#[derive(Clone)]
pub struct AppState {
    pub llm: Arc<FailoverRouter>,
    pub github_hooks: GithubHookState,
    /// Same runtime as Socket Mode. Required for `POST /e2e/message`.
    pub operator: Option<Arc<SlackRuntime>>,
}

impl AppState {
    #[must_use]
    pub const fn new(
        llm: Arc<FailoverRouter>,
        github_hooks: GithubHookState,
        operator: Option<Arc<SlackRuntime>>,
    ) -> Self {
        Self {
            llm,
            github_hooks,
            operator,
        }
    }

    /// Empty router for unit tests that only hit `/health`.
    #[must_use]
    pub fn empty() -> Self {
        use crate::llm::router::TaskChains;
        use std::collections::HashMap;
        Self {
            llm: Arc::new(FailoverRouter::new(HashMap::new(), TaskChains::new())),
            github_hooks: GithubHookState::default(),
            operator: None,
        }
    }
}

/// Tor enrich queue + drip side signals for TUI / operators (same math as GR MCP).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EnrichStatusSnapshot {
    pub pending: u64,
    pub in_flight: u64,
    pub ok: u64,
    pub failed: u64,
    pub skip: u64,
    pub none: u64,
    /// `pending + failed + in_flight` (MCP `queue_remaining_estimate`). Not `ok` / `skip`.
    pub queue_remaining: u64,
    pub next_enrich_after: Option<String>,
    pub wall_streak: Option<u32>,
    pub last_wall_source_id: Option<i64>,
    pub enrich_pid: Option<i32>,
    pub enrich_running: bool,
}

impl EnrichStatusSnapshot {
    #[must_use]
    fn from_counts(counts: EnrichStatusCounts, state_db_path: &Path) -> Self {
        let side = read_enrich_side_signals(state_db_path);
        let queue_remaining = counts.pending + counts.failed + counts.in_flight;
        Self {
            pending: counts.pending,
            in_flight: counts.in_flight,
            ok: counts.ok,
            failed: counts.failed,
            skip: counts.skip,
            none: counts.none,
            queue_remaining,
            next_enrich_after: counts.next_enrich_after,
            wall_streak: side.wall_streak,
            last_wall_source_id: side.last_wall_source_id,
            enrich_pid: side.enrich_pid,
            enrich_running: side.enrich_running,
        }
    }
}

/// Load enrich snapshot from Slack's open DB, else open `state_db_path` briefly.
fn load_enrich_snapshot(state: &AppState) -> Option<EnrichStatusSnapshot> {
    if let Some(op) = &state.operator {
        if let Ok(db) = op.sources.lock() {
            if let Ok(counts) = db.enrich_status_counts() {
                return Some(EnrichStatusSnapshot::from_counts(
                    counts,
                    &op.config.state_db_path,
                ));
            }
        }
    }
    let path = Path::new(state.github_hooks.state_db_path.as_str());
    if !path.exists() {
        return None;
    }
    let db = SourceDb::open(path).ok()?;
    let counts = db.enrich_status_counts().ok()?;
    Some(EnrichStatusSnapshot::from_counts(counts, path))
}

/// JSON snapshot of provider pool + failover routes + last BAT wake.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeStatus {
    pub providers: Vec<String>,
    pub freeform_route_head: String,
    pub freeform_route: String,
    pub load_route_head: String,
    pub load_route: String,
    pub draft_route_head: String,
    pub draft_route: String,
    pub github_webhook_configured: bool,
    pub e2e_inject_ready: bool,
    /// Effective `LinkedIn` company-page publish mode (`config.toml` / env / fallback).
    pub linkedin_publish_mode: String,
    /// Effective X publish mode (`ITCY_X_PUBLISH_MODE`, default playground).
    pub x_publish_mode: String,
    pub last_bat_wake: Option<BatWakeSnapshot>,
    /// Last GitHub delivery that reached the handler.
    pub last_github_delivery: Option<GithubDeliverySnapshot>,
    /// Tunnel / delivery warn (HMAC, 502, wrong path) for TUI.
    pub github_delivery_warn: Option<String>,
    /// Tor enrich queue (null when state DB unavailable).
    pub enrich: Option<EnrichStatusSnapshot>,
    /// Tor SOCKS + control listen (required for Slack `/enrich`).
    pub tor: TorListenStatus,
    /// Local `LinkedIn` HTTP MCP listen (required for production ship / comment reply).
    pub linkedin_mcp: LinkedInMcpStatus,
    /// Twitter/X Brave gold vault (or Bearer) for digest discovery.
    pub twitter: TwitterVaultStatus,
}

impl RuntimeStatus {
    #[must_use]
    pub fn from_state(state: &AppState) -> Self {
        let last_bat_wake = state
            .github_hooks
            .last_wake
            .lock()
            .ok()
            .and_then(|g| g.clone());
        let linkedin_publish_mode = crate::publish::resolve_publish_mode_agile(
            state.github_hooks.publish_mode_fallback.as_str(),
        )
        .map_or_else(
            |_| state.github_hooks.publish_mode_fallback.to_string(),
            |m| m.as_str().to_string(),
        );
        let x_publish_mode = crate::publish::resolve_x_publish_mode("playground")
            .map_or_else(|_| "playground".into(), |m| m.as_str().to_string());
        Self {
            providers: state
                .llm
                .provider_ids()
                .into_iter()
                .map(str::to_string)
                .collect(),
            freeform_route_head: state.llm.route_head_label(TaskKind::Freeform),
            freeform_route: state.llm.route(TaskKind::Freeform),
            load_route_head: state.llm.route_head_label(TaskKind::Load),
            load_route: state.llm.route(TaskKind::Load),
            draft_route_head: state.llm.route_head_label(TaskKind::Draft),
            draft_route: state.llm.route(TaskKind::Draft),
            github_webhook_configured: state.github_hooks.is_configured(),
            e2e_inject_ready: state.operator.is_some(),
            linkedin_publish_mode,
            x_publish_mode,
            last_bat_wake,
            last_github_delivery: state.github_hooks.last_delivery_snapshot(),
            github_delivery_warn: state.github_hooks.delivery_warn_snapshot(),
            enrich: load_enrich_snapshot(state),
            tor: probe_tor_listen(),
            linkedin_mcp: probe_linkedin_mcp(),
            twitter: probe_twitter_vault(),
        }
    }
}

/// Returns a plain `ok` body for liveness probes.
pub async fn health() -> &'static str {
    "ok"
}

/// Returns provider pool + freeform/draft routes + BAT wake (no secrets).
pub async fn status(State(state): State<AppState>) -> Json<RuntimeStatus> {
    Json(RuntimeStatus::from_state(&state))
}

/// Localhost inject: simulate a `#itcy` freeform message (agents / e2e only; not Slack).
#[derive(Debug, Deserialize)]
pub struct E2eMessageRequest {
    pub text: String,
}

/// Localhost inject: simulate a slash command intent (TUI / scripts; not Slack).
#[derive(Debug, Deserialize)]
pub struct E2eSlashRequest {
    pub command: String,
    #[serde(default)]
    pub text: String,
}

/// Localhost inject: ship a company post (agents / e2e). Defaults to **mock**.
#[derive(Debug, Deserialize)]
pub struct E2ePublishRequest {
    pub body: String,
    #[serde(default)]
    pub draft_id: Option<String>,
    #[serde(default)]
    pub pubs_pr_number: Option<u64>,
    /// `playground` (default) or `production`. Per-call only; does not change process config.
    #[serde(default = "default_e2e_publish_mode")]
    pub mode: String,
    #[serde(default)]
    pub quote_tweet_id: Option<String>,
}

fn default_e2e_publish_mode() -> String {
    "playground".into()
}

/// JSON body returned by `POST /e2e/publish`.
#[derive(Debug, Serialize)]
pub struct E2ePublishResponse {
    pub ok: bool,
    pub source: &'static str,
    pub mode: String,
    pub detail: String,
    pub linkedin_urn: Option<String>,
    pub linkedin_url: Option<String>,
    pub audit_error: Option<String>,
}

/// Localhost inject response for freeform operator text.
#[derive(Debug, Serialize)]
pub struct E2eMessageResponse {
    pub ok: bool,
    pub source: &'static str,
    pub reply: String,
}

/// Localhost inject response for slash commands (includes ack when applicable).
#[derive(Debug, Serialize)]
pub struct E2eSlashResponse {
    pub ok: bool,
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack: Option<String>,
    pub reply: String,
}

/// `POST /e2e/message` - inject freeform operator text (localhost agents only).
///
/// Not a Slack test. Bot Socket Mode messages are ignored (`bot_id`); agents use this
/// to exercise the freeform path without posting as the bot.
///
/// # Errors
///
/// Returns a non-success [`StatusCode`] when the inject path rejects the request.
pub async fn e2e_message(
    State(state): State<AppState>,
    Json(body): Json<E2eMessageRequest>,
) -> Result<Json<E2eMessageResponse>, StatusCode> {
    let text = body.text.trim();
    if text.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let Some(op) = state.operator.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let reply = op.handle_operator_text(text).await;
    Ok(Json(E2eMessageResponse {
        ok: true,
        source: "e2e",
        reply,
    }))
}

/// `POST /entrypoint/slash` - inject slash command intent (localhost TUI / scripts).
///
/// Runs the same `dispatch_command` path as Socket Mode. Returns `{ack, reply}` in JSON.
/// Does **not** call Slack `chat.postMessage` (no channel posts, no production noise).
/// Live Socket Mode posts ack **before** work via `handle_slash_in_channel`; inject returns
/// both fields after work for the TUI and scripts to assert.
///
/// # Errors
///
/// Returns a non-success [`StatusCode`] when the inject path rejects the request.
pub async fn entrypoint_slash(
    State(state): State<AppState>,
    Json(body): Json<E2eSlashRequest>,
) -> Result<Json<E2eSlashResponse>, StatusCode> {
    let command = body.command.trim();
    if command.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let Some(op) = state.operator.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let outcome = op.handle_operator_slash(command, body.text.trim()).await;
    Ok(Json(E2eSlashResponse {
        ok: true,
        source: "inject-slash",
        ack: outcome.ack,
        reply: outcome.reply,
    }))
}

/// `POST /e2e/publish` - ship company post (localhost). Default mode **mock** per call.
///
/// Does not change process-wide config. Use `mode=live` only when intentionally
/// testing with a CM token; leave mock for e2e so production `LinkedIn` stays clean.
///
/// # Errors
///
/// Returns a non-success [`StatusCode`] when the inject path rejects the request.
pub async fn e2e_publish(
    State(state): State<AppState>,
    Json(body): Json<E2ePublishRequest>,
) -> Result<Json<E2ePublishResponse>, StatusCode> {
    let text = body.body.trim();
    if text.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mode =
        crate::publish::PublishMode::parse(&body.mode).map_err(|_| StatusCode::BAD_REQUEST)?;
    let draft_id = body
        .draft_id
        .filter(|s| !s.trim().is_empty())
        .or_else(|| crate::publish::draft_id_from_body(text));
    let is_x = draft_id
        .as_deref()
        .is_some_and(|id| id.starts_with("TWEET-") || id.starts_with("XPOST-"))
        || text.contains("Tweet ID:")
        || text.contains("XPOST ID:");
    if is_x {
        let request = crate::publish::XPublishRequest {
            tweet_id: draft_id,
            pubs_pr_number: body.pubs_pr_number,
            body: text.to_string(),
            quote_tweet_id: body.quote_tweet_id.filter(|s| !s.trim().is_empty()),
        };
        return match crate::publish::ship_x_post(
            state.github_hooks.state_db_path.as_str(),
            "playground",
            request,
            Some(mode),
        )
        .await
        {
            Ok(r) => Ok(Json(E2ePublishResponse {
                ok: true,
                source: "e2e-publish",
                mode: r.mode.as_str().to_string(),
                detail: r.detail,
                linkedin_urn: r.linkedin_urn,
                linkedin_url: r.linkedin_url,
                audit_error: None,
            })),
            Err(e) => Ok(Json(E2ePublishResponse {
                ok: false,
                source: "e2e-publish",
                mode: mode.as_str().to_string(),
                detail: e.to_string(),
                linkedin_urn: None,
                linkedin_url: None,
                audit_error: Some(e.to_string()),
            })),
        };
    }
    let request = crate::publish::PublishRequest {
        draft_id,
        pubs_pr_number: body.pubs_pr_number,
        body: text.to_string(),
    };
    match crate::publish::ship_company_post(
        state.github_hooks.state_db_path.as_str(),
        state.github_hooks.publish_mode_fallback.as_str(),
        request,
        crate::publish::ShipOptions {
            mode_override: Some(mode),
        },
    )
    .await
    {
        Ok(r) => Ok(Json(E2ePublishResponse {
            ok: true,
            source: "e2e-publish",
            mode: r.mode.as_str().to_string(),
            detail: r.detail,
            linkedin_urn: r.linkedin_urn,
            linkedin_url: r.linkedin_url,
            audit_error: None,
        })),
        Err(e) => Ok(Json(E2ePublishResponse {
            ok: false,
            source: "e2e-publish",
            mode: mode.as_str().to_string(),
            detail: e.to_string(),
            linkedin_urn: None,
            linkedin_url: None,
            audit_error: Some(e.to_string()),
        })),
    }
}

/// Router: `GET /health`, `GET /status`, localhost inject routes, GitHub webhook.
pub fn router(state: AppState) -> Router {
    let hooks = state.github_hooks.clone();
    Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/e2e/message", post(e2e_message))
        .route("/entrypoint/slash", post(entrypoint_slash))
        .route("/e2e/publish", post(e2e_publish))
        .with_state(state)
        .merge(
            Router::new()
                // Wake from org ingress (HMAC already verified there).
                .route("/hooks/github", post(hooks_github_wake))
                .with_state(hooks),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn e2e_without_operator_is_503() {
        let app = router(AppState::empty());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/e2e/message")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"text":"help"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn e2e_empty_text_is_400() {
        let app = router(AppState::empty());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/e2e/message")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"text":"  "}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn entrypoint_slash_empty_command_is_400() {
        let app = router(AppState::empty());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/entrypoint/slash")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"command":"  ","text":""}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn entrypoint_slash_without_operator_is_503() {
        let app = router(AppState::empty());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/entrypoint/slash")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"command":"/status_itcy","text":""}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn status_reports_e2e_inject_ready_false_when_empty() {
        let st = RuntimeStatus::from_state(&AppState::empty());
        assert!(!st.e2e_inject_ready);
    }

    #[test]
    fn status_enrich_snapshot_from_temp_db() {
        use crate::sources::store::{InsertSource, SourceDb};
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("runtime.db");
        let db = SourceDb::open(&db_path).expect("open");
        db.insert_source(&InsertSource {
            kind: "linkedin_export",
            activity: "post",
            subject: "s",
            title: "t",
            url: Some("https://www.linkedin.com/feed/update/urn:li:activity:1"),
            raw_text: "Post\nhttps://www.linkedin.com/feed/update/urn:li:activity:1",
            occurred_at: None,
        })
        .expect("insert")
        .expect("id");
        db.ensure_enrich_stub_queue().expect("queue");
        std::fs::write(
            dir.path().join("enrich-wall-streak.txt"),
            "updated_at=x\nwall_streak=2\nlast_wall_source_id=9\n",
        )
        .expect("streak");
        std::fs::write(dir.path().join("enrich-linkedin-urls.pid"), "1\n").expect("pid");

        let mut state = AppState::empty();
        state.github_hooks = state
            .github_hooks
            .with_ship_context(db_path.to_string_lossy().into(), "playground".into());
        let st = RuntimeStatus::from_state(&state);
        let enrich = st.enrich.expect("enrich present");
        assert_eq!(enrich.pending, 1);
        assert_eq!(enrich.queue_remaining, 1);
        assert_eq!(enrich.wall_streak, Some(2));
        assert_eq!(enrich.last_wall_source_id, Some(9));
        assert_eq!(enrich.enrich_pid, Some(1));
        assert!(enrich.enrich_running, "pid 1 should be alive on Linux");
    }

    #[tokio::test]
    async fn e2e_publish_playground_ok() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("p.db");
        let mut state = AppState::empty();
        state.github_hooks = state
            .github_hooks
            .with_ship_context(db.to_string_lossy().into(), "production".into());
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/e2e/publish")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"body":"Draft ID: DRAFT-E2E-000001\n\nhello playground","mode":"playground"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(v.get("ok"), Some(&serde_json::Value::Bool(true)));
        assert_eq!(
            v.get("mode"),
            Some(&serde_json::Value::String("playground".into()))
        );
    }
}

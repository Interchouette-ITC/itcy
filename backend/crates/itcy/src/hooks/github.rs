// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! GitHub wake for publications PRs and related events.
//!
//! Delivery: GitHub → org ingress (`POST /github/webhook_ITC`, HMAC there)
//! → forward → `POST /hooks/github` on `ITCy` (:4700). No HMAC here (ingress verified).
//! Prefer loopback callers only (`ConnectInfo` peer check).
//!
//! Subscribed events (GitHub UI / org hook): `pull_request_review`, `pull_request`,
//! `pull_request_review_comment`, `pull_request_review_thread`, `issue_comment`,
//! `push`. Conversation-tab PR comments arrive as `issue_comment` (GitHub models PRs as issues).

use crate::bat::github::{BatGithubConfig, GithubClient};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

/// Shared wake state (last wake / delivery for `/status`).
#[derive(Clone, Default)]
pub struct GithubHookState {
    /// Org publications repo (`owner/repo`) - Posts only.
    pub publications_full_name: Arc<String>,
    /// Interchouette fork (`owner/repo`) - Draft PRs + Approve wake.
    pub drafts_full_name: Arc<String>,
    /// App state DB path for publish audit (`runtime.db`).
    pub state_db_path: Arc<String>,
    /// Boot snapshot of publish mode; ship re-resolves env/config each time.
    pub publish_mode_fallback: Arc<String>,
    /// Last accepted BAT wake (for `/status`; no secrets).
    pub last_wake: Arc<Mutex<Option<BatWakeSnapshot>>>,
    /// Last delivery that reached the handler (including rejects).
    pub last_delivery: Arc<Mutex<Option<GithubDeliverySnapshot>>>,
    /// Last delivery / tunnel warn (forward errors, 502, wrong path, …).
    pub delivery_warn: Arc<Mutex<Option<String>>>,
}

impl GithubHookState {
    /// Builds state from env (BAT org/fork/repo).
    #[must_use]
    pub fn from_env() -> Self {
        let org = std::env::var("ITCY_BAT_ORG_OWNER")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "Interchouette-ITC".into());
        let fork = std::env::var("ITCY_BAT_FORK_OWNER")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "Interchouette".into());
        let repo = std::env::var("ITCY_BAT_REPO")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "itcy-publications".into());
        Self {
            publications_full_name: Arc::new(format!("{org}/{repo}")),
            drafts_full_name: Arc::new(format!("{fork}/{repo}")),
            state_db_path: Arc::new("../sql/runtime.db".into()),
            publish_mode_fallback: Arc::new("playground".into()),
            last_wake: Arc::new(Mutex::new(None)),
            last_delivery: Arc::new(Mutex::new(None)),
            delivery_warn: Arc::new(Mutex::new(None)),
        }
    }

    /// Sets state DB + publish-mode fallback used after BAT merge ship.
    #[must_use]
    pub fn with_ship_context(
        mut self,
        state_db_path: String,
        publish_mode_fallback: String,
    ) -> Self {
        self.state_db_path = Arc::new(state_db_path);
        self.publish_mode_fallback = Arc::new(publish_mode_fallback);
        self
    }

    /// Wake is always mounted; HMAC lives on org ingress.
    #[must_use]
    pub const fn is_configured(&self) -> bool {
        true
    }

    fn store_wake(&self, snap: BatWakeSnapshot) {
        if let Ok(mut guard) = self.last_wake.lock() {
            *guard = Some(snap);
        }
    }

    /// Records a delivery outcome for `GET /status` (no secrets).
    pub fn record_delivery(&self, delivery: Delivery<'_>, outcome: &str, http_status: u16) {
        let event = if delivery.event.is_empty() {
            "-".into()
        } else {
            delivery.event.to_string()
        };
        let snap = GithubDeliverySnapshot {
            at_unix: chrono::Local::now().timestamp(),
            event,
            delivery_id: delivery.id.to_string(),
            outcome: outcome.to_string(),
            http_status,
        };
        if let Ok(mut guard) = self.last_delivery.lock() {
            *guard = Some(snap);
        }
        if outcome == "ok" || outcome == "ignored" {
            if outcome == "ok" {
                if let Ok(mut guard) = self.delivery_warn.lock() {
                    *guard = None;
                }
            }
        } else {
            let msg = match outcome {
                "reject_peer" => "non-loopback peer".to_string(),
                "reject_secret" => "secret unset".to_string(),
                "reject_hmac" => "HMAC reject".to_string(),
                "error" => format!("error status={http_status}"),
                other => other.to_string(),
            };
            self.set_delivery_warn(msg);
        }
    }

    /// Sets the delivery warn line (handler rejects or ngrok inspect).
    pub fn set_delivery_warn(&self, msg: impl Into<String>) {
        if let Ok(mut guard) = self.delivery_warn.lock() {
            *guard = Some(msg.into());
        }
    }

    /// Last delivery snapshot for `/status`.
    #[must_use]
    pub fn last_delivery_snapshot(&self) -> Option<GithubDeliverySnapshot> {
        self.last_delivery.lock().ok().and_then(|g| g.clone())
    }

    /// Current delivery warn for `/status`.
    #[must_use]
    pub fn delivery_warn_snapshot(&self) -> Option<String> {
        self.delivery_warn.lock().ok().and_then(|g| g.clone())
    }
}

/// Public snapshot of the last BAT wake (safe for `/status`).
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct BatWakeSnapshot {
    pub at_unix: i64,
    pub repo: String,
    pub pr_number: u64,
    pub reviewer: String,
    pub action: String,
    pub merged: bool,
    pub detail: String,
}

/// Last GitHub delivery that reached `ITCy` (safe for `/status`; no secrets).
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct GithubDeliverySnapshot {
    pub at_unix: i64,
    pub event: String,
    pub delivery_id: String,
    /// `ok` | `reject_secret` | `reject_hmac` | `ignored` | `error`
    pub outcome: String,
    pub http_status: u16,
}

/// Correlation ids from GitHub delivery headers (safe to log).
#[derive(Debug, Clone, Copy)]
pub struct Delivery<'a> {
    pub id: &'a str,
    pub event: &'a str,
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

/// `POST /hooks/github`: wake from org ingress (BAT + babysit). No HMAC.
pub async fn hooks_github_wake(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<GithubHookState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let delivery = Delivery {
        id: header_str(&headers, "x-github-delivery"),
        event: header_str(&headers, "x-github-event"),
    };

    if !peer.ip().is_loopback() {
        warn!(
            delivery = delivery.id,
            event = delivery.event,
            peer = %peer,
            "hooks/github: reject (non-loopback)"
        );
        state.record_delivery(delivery, "reject_peer", 403);
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"ok": false, "error": "loopback only"})),
        )
            .into_response();
    }

    info!(
        delivery = delivery.id,
        event = delivery.event,
        bytes = body.len(),
        "hooks/github: delivery"
    );

    if delivery.event == "ping" {
        info!(delivery = delivery.id, "hooks/github: ping ok");
        state.record_delivery(delivery, "ok", 200);
        return (StatusCode::OK, Json(json!({"ok": true, "pong": true}))).into_response();
    }

    let response = match delivery.event {
        "pull_request_review" => handle_pull_request_review(&state, delivery, &body),
        "pull_request" => handle_pull_request(&state, delivery, &body).await,
        "pull_request_review_thread" => handle_review_thread(&state, delivery, &body),
        "pull_request_review_comment" => handle_review_comment(&state, delivery, &body),
        "issue_comment" => handle_issue_comment(&state, delivery, &body).await,
        "push" => handle_push(&state, delivery, &body),
        other => {
            info!(
                delivery = delivery.id,
                event = other,
                "hooks/github: ignore (unsubscribed event)"
            );
            state.record_delivery(delivery, "ignored", 200);
            return (
                StatusCode::OK,
                Json(json!({"ok": true, "ignored": true, "event": other})),
            )
                .into_response();
        }
    };

    let status = response.status().as_u16();
    let outcome = if (200..300).contains(&status) {
        "ok"
    } else {
        "error"
    };
    state.record_delivery(delivery, outcome, status);
    response
}

fn handle_pull_request_review(
    state: &GithubHookState,
    delivery: Delivery<'_>,
    body: &[u8],
) -> axum::response::Response {
    let payload: ReviewPayload = match parse_webhook_json(body) {
        Ok(p) => p,
        Err(e) => return webhook_parse_failed(delivery, e),
    };

    let full_name = match require_allowed_repo(state, payload.repository.as_ref()) {
        Ok(n) => n,
        Err(reason) => return ignored(delivery, &reason),
    };

    let review_state = payload
        .review
        .as_ref()
        .map_or("", |r| r.state.as_str())
        .to_ascii_lowercase();
    let pr_number = payload.pull_request.as_ref().map_or(0, |p| p.number);
    let base_ref = payload
        .pull_request
        .as_ref()
        .and_then(|p| p.base.as_ref())
        .and_then(|b| b.ref_.as_deref())
        .unwrap_or("");
    let reviewer = payload
        .review
        .as_ref()
        .and_then(|r| r.user.as_ref())
        .map(|u| u.login.clone())
        .unwrap_or_default();

    if review_state != "approved" {
        return babysit_non_approved_review(
            state,
            delivery,
            full_name,
            pr_number,
            &reviewer,
            &review_state,
        );
    }

    info!(
        delivery = delivery.id,
        repo = %full_name,
        pr = pr_number,
        base = base_ref,
        reviewer = %reviewer,
        "hooks/github: BAT Approve wake"
    );

    // Promote can take many seconds (GitHub API). Ingress used to time out at 2s and
    // cancel this future mid-merge. Ack immediately; finish BAT on a background task.
    let state_bg = state.clone();
    let delivery_id = delivery.id.to_string();
    let reviewer_bg = reviewer.clone();
    tokio::spawn(async move {
        let (merged, detail) = merge_outcome(&state_bg, &full_name, pr_number).await;
        info!(
            delivery = delivery_id.as_str(),
            repo = %full_name,
            pr = pr_number,
            merged,
            detail = %detail,
            "hooks/github: BAT merge outcome"
        );
        record_wake(
            &state_bg,
            full_name,
            pr_number,
            reviewer_bg,
            "approved".into(),
            merged,
            detail,
        );
    });

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "woke": true,
            "pr": pr_number,
            "reviewer": reviewer,
            "queued": true,
            "detail": "BAT promote queued",
        })),
    )
        .into_response()
}

fn webhook_parse_failed(
    delivery: Delivery<'_>,
    error: impl std::fmt::Display,
) -> axum::response::Response {
    warn!(
        delivery = delivery.id,
        event = delivery.event,
        error = %error,
        "hooks/github: parse failed"
    );
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"ok": false, "error": "invalid json"})),
    )
        .into_response()
}

fn parse_webhook_json<T: DeserializeOwned>(body: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(body)
}

fn require_allowed_repo(
    state: &GithubHookState,
    repo: Option<&RepoBody>,
) -> Result<String, String> {
    let Some(full_name) = repo.map(repo_full_name) else {
        return Err("missing repo".into());
    };
    if !repo_allowed(state, &full_name) {
        return Err(format!("repo filter ({full_name})"));
    }
    Ok(full_name)
}

fn record_wake(
    state: &GithubHookState,
    repo: String,
    pr_number: u64,
    reviewer: String,
    action: String,
    merged: bool,
    detail: String,
) {
    state.store_wake(BatWakeSnapshot {
        at_unix: chrono::Local::now().timestamp(),
        repo,
        pr_number,
        reviewer,
        action,
        merged,
        detail,
    });
}

fn babysit_ok(fields: &serde_json::Value) -> axum::response::Response {
    let mut body = json!({"ok": true, "babysit": true});
    if let (Some(base), Some(extra)) = (body.as_object_mut(), fields.as_object()) {
        for (k, v) in extra {
            base.insert(k.clone(), v.clone());
        }
    }
    (StatusCode::OK, Json(body)).into_response()
}

fn babysit_non_approved_review(
    state: &GithubHookState,
    delivery: Delivery<'_>,
    full_name: String,
    pr_number: u64,
    reviewer: &str,
    review_state: &str,
) -> axum::response::Response {
    info!(
        delivery = delivery.id,
        repo = %full_name,
        pr = pr_number,
        reviewer = %reviewer,
        state = %review_state,
        "hooks/github: babysit review"
    );
    record_wake(
        state,
        full_name,
        pr_number,
        reviewer.to_string(),
        format!("review:{review_state}"),
        false,
        format!("babysit review state={review_state}"),
    );
    babysit_ok(&json!({
        "event": "pull_request_review",
        "pr": pr_number,
        "review_state": review_state,
    }))
}

async fn handle_pull_request(
    state: &GithubHookState,
    delivery: Delivery<'_>,
    body: &[u8],
) -> axum::response::Response {
    let payload: PullPayload = match parse_webhook_json(body) {
        Ok(p) => p,
        Err(e) => return webhook_parse_failed(delivery, e),
    };

    let full_name = match require_allowed_repo(state, payload.repository.as_ref()) {
        Ok(n) => n,
        Err(reason) => return ignored(delivery, &reason),
    };

    let action = payload.action.unwrap_or_default();
    let pr_number = payload.pull_request.as_ref().map_or(0, |p| p.number);
    let title = payload
        .pull_request
        .as_ref()
        .and_then(|p| p.title.clone())
        .unwrap_or_default();

    info!(
        delivery = delivery.id,
        repo = %full_name,
        pr = pr_number,
        action = %action,
        title = %title,
        "hooks/github: babysit pull_request"
    );

    // Re-check BAT gate when the PR changes or becomes reviewable again.
    let try_merge = matches!(
        action.as_str(),
        "opened" | "reopened" | "synchronize" | "ready_for_review" | "review_request_removed"
    );

    let (merged, detail) = if try_merge && pr_number > 0 {
        let outcome = merge_outcome(state, &full_name, pr_number).await;
        info!(
            delivery = delivery.id,
            repo = %full_name,
            pr = pr_number,
            merged = outcome.0,
            detail = %outcome.1,
            "hooks/github: BAT merge outcome"
        );
        outcome
    } else if action == "closed" {
        let merged_flag = payload
            .pull_request
            .as_ref()
            .and_then(|p| p.merged)
            .unwrap_or(false);
        (
            merged_flag,
            if merged_flag {
                format!("PR #{pr_number} closed (merged)")
            } else {
                format!("PR #{pr_number} closed (not merged)")
            },
        )
    } else {
        (
            false,
            format!("babysit pull_request action={action} pr=#{pr_number}"),
        )
    };

    record_wake(
        state,
        full_name,
        pr_number,
        String::new(),
        format!("pull_request:{action}"),
        merged,
        detail.clone(),
    );

    babysit_ok(&json!({
        "event": "pull_request",
        "action": action,
        "pr": pr_number,
        "merged": merged,
        "detail": detail,
    }))
}

fn handle_review_thread(
    state: &GithubHookState,
    delivery: Delivery<'_>,
    body: &[u8],
) -> axum::response::Response {
    let payload: ThreadPayload = match parse_webhook_json(body) {
        Ok(p) => p,
        Err(e) => return webhook_parse_failed(delivery, e),
    };

    let full_name = match require_allowed_repo(state, payload.repository.as_ref()) {
        Ok(n) => n,
        Err(reason) => return ignored(delivery, &reason),
    };

    let action = payload.action.unwrap_or_default();
    let pr_number = payload.pull_request.as_ref().map_or(0, |p| p.number);
    let resolved = payload
        .thread
        .as_ref()
        .and_then(|t| t.is_resolved)
        .unwrap_or(false);

    info!(
        delivery = delivery.id,
        repo = %full_name,
        pr = pr_number,
        action = %action,
        resolved,
        "hooks/github: babysit review thread"
    );

    record_wake(
        state,
        full_name,
        pr_number,
        String::new(),
        format!("review_thread:{action}"),
        false,
        format!("thread action={action} resolved={resolved}"),
    );

    babysit_ok(&json!({
        "event": "pull_request_review_thread",
        "action": action,
        "pr": pr_number,
        "resolved": resolved,
    }))
}

fn handle_review_comment(
    state: &GithubHookState,
    delivery: Delivery<'_>,
    body: &[u8],
) -> axum::response::Response {
    let payload: ReviewCommentPayload = match parse_webhook_json(body) {
        Ok(p) => p,
        Err(e) => return webhook_parse_failed(delivery, e),
    };

    let full_name = match require_allowed_repo(state, payload.repository.as_ref()) {
        Ok(n) => n,
        Err(reason) => return ignored(delivery, &reason),
    };

    let action = payload.action.unwrap_or_default();
    let pr_number = payload.pull_request.as_ref().map_or(0, |p| p.number);
    let user = payload
        .comment
        .as_ref()
        .and_then(|c| c.user.as_ref())
        .map(|u| u.login.clone())
        .unwrap_or_default();
    let path = payload
        .comment
        .as_ref()
        .and_then(|c| c.path.clone())
        .unwrap_or_default();
    let comment_id = payload.comment.as_ref().and_then(|c| c.id).unwrap_or(0);
    let preview = payload
        .comment
        .as_ref()
        .and_then(|c| c.body.as_deref())
        .map(preview_text)
        .unwrap_or_default();

    info!(
        delivery = delivery.id,
        event = "pull_request_review_comment",
        repo = %full_name,
        pr = pr_number,
        action = %action,
        user = %user,
        path = %path,
        comment_id,
        preview = %preview,
        "hooks/github: pull_request_review_comment (diff line on PR)"
    );

    record_wake(
        state,
        full_name,
        pr_number,
        user.clone(),
        format!("review_comment:{action}"),
        false,
        format!("pull_request_review_comment action={action} by {user} path={path}"),
    );

    babysit_ok(&json!({
        "event": "pull_request_review_comment",
        "action": action,
        "pr": pr_number,
        "user": user,
        "path": path,
        "comment_id": comment_id,
    }))
}

/// Conversation-tab comment on a PR. GitHub names this `issue_comment` (PRs are issues).
async fn handle_issue_comment(
    state: &GithubHookState,
    delivery: Delivery<'_>,
    body: &[u8],
) -> axum::response::Response {
    let payload: IssueCommentPayload = match parse_webhook_json(body) {
        Ok(p) => p,
        Err(e) => return webhook_parse_failed(delivery, e),
    };

    let full_name = match require_allowed_repo(state, payload.repository.as_ref()) {
        Ok(n) => n,
        Err(reason) => return ignored(delivery, &reason),
    };

    let Some(issue) = payload.issue.as_ref() else {
        return ignored(delivery, "missing issue");
    };
    // Plain issues have no pull_request object; only PR conversation comments wake us.
    if issue.pull_request.is_none() {
        return ignored(delivery, "not a PR (plain issue)");
    }

    let action = payload.action.unwrap_or_default();
    let pr_number = issue.number;
    let user = payload
        .comment
        .as_ref()
        .and_then(|c| c.user.as_ref())
        .map(|u| u.login.clone())
        .unwrap_or_default();
    let comment_id = payload.comment.as_ref().and_then(|c| c.id).unwrap_or(0);
    let preview = payload
        .comment
        .as_ref()
        .and_then(|c| c.body.as_deref())
        .map(preview_text)
        .unwrap_or_default();

    info!(
        delivery = delivery.id,
        event = "issue_comment",
        repo = %full_name,
        pr = pr_number,
        action = %action,
        user = %user,
        comment_id,
        preview = %preview,
        "hooks/github: issue_comment (PR conversation)"
    );

    // Re-check BAT when someone comments on the PR conversation.
    let (merged, detail) = if action == "created" && pr_number > 0 {
        let outcome = merge_outcome(state, &full_name, pr_number).await;
        info!(
            delivery = delivery.id,
            repo = %full_name,
            pr = pr_number,
            merged = outcome.0,
            detail = %outcome.1,
            "hooks/github: BAT merge outcome"
        );
        outcome
    } else {
        (
            false,
            format!("babysit issue_comment action={action} pr=#{pr_number}"),
        )
    };

    record_wake(
        state,
        full_name,
        pr_number,
        user.clone(),
        format!("issue_comment:{action}"),
        merged,
        detail.clone(),
    );

    babysit_ok(&json!({
        "event": "issue_comment",
        "action": action,
        "pr": pr_number,
        "user": user,
        "comment_id": comment_id,
        "merged": merged,
        "detail": detail,
    }))
}

fn handle_push(
    state: &GithubHookState,
    delivery: Delivery<'_>,
    body: &[u8],
) -> axum::response::Response {
    let payload: PushPayload = match parse_webhook_json(body) {
        Ok(p) => p,
        Err(e) => return webhook_parse_failed(delivery, e),
    };

    let full_name = match require_allowed_repo(state, payload.repository.as_ref()) {
        Ok(n) => n,
        Err(reason) => return ignored(delivery, &reason),
    };

    let reference = payload.reference.unwrap_or_default();
    info!(
        delivery = delivery.id,
        repo = %full_name,
        reference = %reference,
        "hooks/github: babysit push"
    );

    record_wake(
        state,
        full_name,
        0,
        String::new(),
        "push".into(),
        false,
        format!("push {reference}"),
    );

    babysit_ok(&json!({
        "event": "push",
        "ref": reference,
    }))
}

fn ignored(delivery: Delivery<'_>, reason: &str) -> axum::response::Response {
    info!(
        delivery = delivery.id,
        event = delivery.event,
        reason = %reason,
        "hooks/github: ignored"
    );
    (
        StatusCode::OK,
        Json(json!({"ok": true, "ignored": true, "reason": reason})),
    )
        .into_response()
}

fn repo_allowed(state: &GithubHookState, full_name: &str) -> bool {
    full_name.eq_ignore_ascii_case(state.publications_full_name.as_str())
        || full_name.eq_ignore_ascii_case(state.drafts_full_name.as_str())
}

fn repo_full_name(repo: &RepoBody) -> String {
    repo.full_name.clone()
}

async fn merge_outcome(state: &GithubHookState, full_name: &str, pr_number: u64) -> (bool, String) {
    if !repo_allowed(state, full_name) {
        return (false, format!("repo filter ({full_name})"));
    }
    let owner = full_name.split('/').next().unwrap_or("");
    match try_promote_if_bat_green(state, owner, pr_number).await {
        Ok(msg) => (true, msg),
        Err(WakeSkip::NotReady(msg) | WakeSkip::Error(msg)) => (false, msg),
    }
}

#[derive(Debug)]
enum WakeSkip {
    NotReady(String),
    Error(String),
}

async fn try_promote_if_bat_green(
    state: &GithubHookState,
    pr_owner: &str,
    pr_number: u64,
) -> Result<String, WakeSkip> {
    if pr_number == 0 {
        return Err(WakeSkip::Error("missing pr number".into()));
    }
    let cfg = BatGithubConfig::from_env().map_err(|e| WakeSkip::Error(e.to_string()))?;
    let posts_base = cfg.posts_base.clone();
    let tweet_posts_base = cfg.tweet_posts_base.clone();
    let client = GithubClient::new(cfg).map_err(|e| WakeSkip::Error(e.to_string()))?;
    let status = client
        .bat_readiness_on(pr_owner, pr_number)
        .await
        .map_err(|e| WakeSkip::Error(e.to_string()))?;
    if !status.approved {
        return Err(WakeSkip::NotReady(format!(
            "waiting Approve from {}",
            status.expected_reviewer
        )));
    }
    let head = client
        .pull_head_ref_on(pr_owner, pr_number)
        .await
        .map_err(|e| WakeSkip::Error(e.to_string()))?;
    if !crate::bat::pack::is_bat_pr_head(&head) {
        return Err(WakeSkip::NotReady(format!(
            "babysit non-BAT PR head `{head}` (expected draft/DRAFT-… or tweet/TWEET-…)"
        )));
    }
    let promoted = client
        .promote_draft_pr_to_org(pr_owner, pr_number)
        .await
        .map_err(|e| WakeSkip::Error(e.to_string()))?;

    let (ship_ok, ship_detail) =
        if promoted.post_id.starts_with("XPOST-") || promoted.draft_id.starts_with("TWEET-") {
            ship_promoted_xpost(state, &promoted).await
        } else {
            ship_promoted_post(state, &promoted).await
        };
    if ship_ok {
        if let Ok(store) = crate::bat::store::DraftStore::open(state.state_db_path.as_str()) {
            let _ = store.mark_status_from(&promoted.draft_id, "accepted", "published");
            let _ = store.mark_status_from(&promoted.draft_id, "open", "published");
        }
    }
    let dest = if promoted.post_id.starts_with("XPOST-") {
        tweet_posts_base.as_str()
    } else {
        posts_base.as_str()
    };
    Ok(format!(
        "promoted {} → {} on `{dest}`; Draft PR #{pr_number} merged; {ship_detail}",
        promoted.draft_id, promoted.post_id
    ))
}

/// Ships the promoted XPOST body (mock/live). Never fails the promote outcome.
async fn ship_promoted_xpost(
    state: &GithubHookState,
    promoted: &crate::bat::github::PromoteResult,
) -> (bool, String) {
    let quote = promoted.quote_tweet_id.trim();
    let request = crate::publish::XPublishRequest {
        tweet_id: Some(promoted.post_id.clone()),
        pubs_pr_number: Some(promoted.fork_pr_number),
        body: promoted.body.clone(),
        quote_tweet_id: if quote.is_empty() {
            None
        } else {
            Some(quote.to_string())
        },
        in_reply_to_tweet_id: None,
    };
    match crate::publish::ship_x_post(state.state_db_path.as_str(), "playground", request, None)
        .await
    {
        Ok(r) => {
            let notice = r.ship_notice_text().to_string();
            maybe_post_ship_notice(&promoted.post_id, &notice).await;
            (true, notice)
        }
        Err(e) => {
            warn!(
                xpost_id = %promoted.post_id,
                error = %e,
                "publish: x ship failed after promote"
            );
            maybe_post_ship_fail(&promoted.post_id, &e.to_string()).await;
            (false, format!("x ship error: {e}"))
        }
    }
}

/// Ships the promoted Post body (mock/live). Never fails the promote outcome: audit records errors.
async fn ship_promoted_post(
    state: &GithubHookState,
    promoted: &crate::bat::github::PromoteResult,
) -> (bool, String) {
    let request = crate::publish::PublishRequest {
        draft_id: Some(promoted.post_id.clone()),
        pubs_pr_number: Some(promoted.fork_pr_number),
        body: promoted.body.clone(),
    };
    match crate::publish::ship_company_post(
        state.state_db_path.as_str(),
        state.publish_mode_fallback.as_str(),
        request,
        crate::publish::ShipOptions::default(),
    )
    .await
    {
        Ok(r) => {
            let notice = r.ship_notice_text().to_string();
            maybe_post_ship_notice(&promoted.post_id, &notice).await;
            (true, notice)
        }
        Err(e) => {
            warn!(
                post_id = %promoted.post_id,
                error = %e,
                "publish: ship failed after promote"
            );
            maybe_post_ship_fail(&promoted.post_id, &e.to_string()).await;
            (false, format!("ship error: {e}"))
        }
    }
}

async fn maybe_post_ship_notice(post_id: &str, detail: &str) {
    crate::slack::api::post_ship_notice(post_id, detail).await;
}

async fn maybe_post_ship_fail(post_id: &str, error: &str) {
    crate::slack::api::post_ship_fail(post_id, error).await;
}

#[derive(Debug, Deserialize)]
struct ReviewPayload {
    review: Option<ReviewBody>,
    pull_request: Option<PullBody>,
    repository: Option<RepoBody>,
}

#[derive(Debug, Deserialize)]
struct PullPayload {
    action: Option<String>,
    pull_request: Option<PullBody>,
    repository: Option<RepoBody>,
}

#[derive(Debug, Deserialize)]
struct ThreadPayload {
    action: Option<String>,
    pull_request: Option<PullBody>,
    repository: Option<RepoBody>,
    thread: Option<ThreadBody>,
}

#[derive(Debug, Deserialize)]
struct ThreadBody {
    is_resolved: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ReviewCommentPayload {
    action: Option<String>,
    pull_request: Option<PullBody>,
    repository: Option<RepoBody>,
    comment: Option<ReviewCommentBody>,
}

#[derive(Debug, Deserialize)]
struct ReviewCommentBody {
    id: Option<u64>,
    path: Option<String>,
    body: Option<String>,
    user: Option<UserBody>,
}

#[derive(Debug, Deserialize)]
struct IssueCommentPayload {
    action: Option<String>,
    issue: Option<IssueBody>,
    comment: Option<IssueCommentBody>,
    repository: Option<RepoBody>,
}

#[derive(Debug, Deserialize)]
struct IssueBody {
    number: u64,
    /// Present when the "issue" is a pull request (conversation tab).
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct IssueCommentBody {
    id: Option<u64>,
    body: Option<String>,
    user: Option<UserBody>,
}

/// Short single-line preview for logs (no secrets expected in review text).
fn preview_text(s: &str) -> String {
    const MAX: usize = 80;
    let flat: String = s
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    if flat.chars().count() <= MAX {
        flat.to_string()
    } else {
        let truncated: String = flat.chars().take(MAX).collect();
        format!("{truncated}...")
    }
}

#[derive(Debug, Deserialize)]
struct PushPayload {
    #[serde(rename = "ref")]
    reference: Option<String>,
    repository: Option<RepoBody>,
}

#[derive(Debug, Deserialize)]
struct ReviewBody {
    state: String,
    user: Option<UserBody>,
}

#[derive(Debug, Deserialize)]
struct UserBody {
    login: String,
}

#[derive(Debug, Deserialize)]
struct PullBody {
    number: u64,
    title: Option<String>,
    merged: Option<bool>,
    base: Option<PullRefBody>,
}

#[derive(Debug, Deserialize)]
struct PullRefBody {
    #[serde(rename = "ref")]
    ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RepoBody {
    full_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_delivery_ok_clears_warn() {
        let state = GithubHookState::default();
        state.set_delivery_warn("non-loopback peer");
        let d = Delivery {
            id: "del-1",
            event: "ping",
        };
        state.record_delivery(d, "ok", 200);
        let snap = state.last_delivery_snapshot().expect("snap");
        assert_eq!(snap.outcome, "ok");
        assert_eq!(snap.event, "ping");
        assert_eq!(snap.http_status, 200);
        assert!(state.delivery_warn_snapshot().is_none());
    }

    #[test]
    fn record_delivery_reject_peer_sets_warn() {
        let state = GithubHookState::default();
        let d = Delivery {
            id: "del-2",
            event: "push",
        };
        state.record_delivery(d, "reject_peer", 403);
        let snap = state.last_delivery_snapshot().expect("snap");
        assert_eq!(snap.outcome, "reject_peer");
        assert_eq!(
            state.delivery_warn_snapshot().as_deref(),
            Some("non-loopback peer")
        );
    }

    #[test]
    fn record_delivery_ignored() {
        let state = GithubHookState::default();
        let d = Delivery {
            id: "del-3",
            event: "label",
        };
        state.record_delivery(d, "ignored", 200);
        let snap = state.last_delivery_snapshot().expect("snap");
        assert_eq!(snap.outcome, "ignored");
        assert!(state.delivery_warn_snapshot().is_none());
    }

    #[test]
    fn wake_always_configured() {
        assert!(GithubHookState::default().is_configured());
    }
}

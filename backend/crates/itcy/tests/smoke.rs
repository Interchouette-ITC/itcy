// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Smoke: server starts and answers health + status.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn server_starts_and_health_returns_ok() {
    let app = itcy::app_empty();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn health_body_is_ok() {
    assert_eq!(itcy::health::health().await, "ok");
}

#[tokio::test]
async fn status_returns_json_shape() {
    let app = itcy::app_empty();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert!(v.get("providers").is_some());
    assert!(v.get("freeform_route").is_some());
    assert!(v.get("draft_route").is_some());
    assert!(v.get("github_webhook_configured").is_some());
    assert!(v.get("e2e_inject_ready").is_some());
    assert!(v.get("linkedin_publish_mode").is_some());
    assert!(v.get("enrich").is_some());
    assert_eq!(
        v.get("e2e_inject_ready"),
        Some(&serde_json::Value::Bool(false))
    );
    assert!(v.get("last_bat_wake").is_some());
    assert!(v.get("last_github_delivery").is_some());
    assert!(v.get("github_delivery_warn").is_some());
    assert!(v.get("tor").is_some());
    assert!(v["tor"].get("ok").is_some());
    assert!(v["tor"].get("detail").is_some());
    assert_eq!(
        v.get("last_github_delivery"),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(
        v.get("github_delivery_warn"),
        Some(&serde_json::Value::Null)
    );
}

#[test]
fn loads_default_config_toml() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.toml");
    let cfg = itcy::config::Config::load(&path).expect("load config");
    assert!(!cfg.server.bind.is_empty(), "server.bind must be set");
    assert_eq!(cfg.slack.events_transport, "socket");
    assert_eq!(cfg.slack.channel_env, "SLACK_ITCY_CHANNEL_ID");
    assert_eq!(cfg.runtime.max_context_messages, 20);
    assert!(!cfg.runtime.state_db_path.is_empty());
    assert!(!cfg.llm.freeform_route.is_empty());
    assert!(!cfg.llm.draft_route.is_empty());
    assert_eq!(cfg.linkedin.publish_mode, "playground");
    assert_eq!(cfg.x.publish_mode, "playground");
}

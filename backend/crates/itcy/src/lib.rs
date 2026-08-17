// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! `ITCy` always-on server library.

pub mod bat;
pub mod config;
pub mod health;
pub mod hooks;
pub mod llm;
pub mod logging;
pub mod memory;
pub mod paths;
pub mod prompts;
pub mod publish;
pub mod slack;
pub mod sources;
pub mod sqlite;
pub mod tools;

use crate::health::AppState;
use axum::Router;

/// Builds the HTTP router served by the binary.
pub fn app(state: AppState) -> Router {
    health::router(state)
}

/// Test helper: app with an empty LLM router.
pub fn app_empty() -> Router {
    app(AppState::empty())
}

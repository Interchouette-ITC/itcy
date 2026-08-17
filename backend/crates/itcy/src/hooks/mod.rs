// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Inbound HTTP hooks (GitHub BAT wake). Slack is not the wake bus.

pub mod github;
pub mod ngrok_inspect;

pub use github::{hooks_github_wake, BatWakeSnapshot, GithubDeliverySnapshot, GithubHookState};

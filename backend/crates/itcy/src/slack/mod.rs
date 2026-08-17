// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Slack runtime for the configured `#itcy` channel (Socket Mode).

pub mod api;
pub mod boot;
pub mod chat;
pub mod commands;
pub mod events;
pub mod filter;
pub mod handler;
pub mod saved;
pub mod socket;
pub mod tweets;
pub mod welcome;

pub use boot::boot_ready_text;
pub use handler::{resolve_slack_runtime, SlackRuntime, SlackRuntimeConfig};

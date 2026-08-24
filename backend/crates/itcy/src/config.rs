// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Loads TOML server config.

use serde::Deserialize;
use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub slack: SlackConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub linkedin: LinkedInConfig,
    #[serde(default)]
    pub x: XConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    /// Bind address for the always-on HTTP listener (e.g. `127.0.0.1:4700`).
    pub bind: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SlackConfig {
    /// `socket` (default) or future `http`.
    #[serde(default = "default_events_transport")]
    pub events_transport: String,
    /// Env var holding the `#itcy` channel id.
    #[serde(default = "default_channel_env")]
    pub channel_env: String,
    /// Env var holding the `#daily-digest` channel id.
    #[serde(default = "default_daily_digest_channel_env")]
    pub daily_digest_channel_env: String,
    /// Env var holding the bot token (`xoxb-...`).
    #[serde(default = "default_bot_token_env")]
    pub bot_token_env: String,
    /// Env var holding the app-level token (`xapp-...`) for Socket Mode.
    #[serde(default = "default_app_token_env")]
    pub app_token_env: String,
}

impl Default for SlackConfig {
    fn default() -> Self {
        Self {
            events_transport: default_events_transport(),
            channel_env: default_channel_env(),
            daily_digest_channel_env: default_daily_digest_channel_env(),
            bot_token_env: default_bot_token_env(),
            app_token_env: default_app_token_env(),
        }
    }
}

fn default_events_transport() -> String {
    "socket".into()
}
fn default_channel_env() -> String {
    "SLACK_ITCY_CHANNEL_ID".into()
}
fn default_daily_digest_channel_env() -> String {
    "SLACK_DAILY_DIGEST_CHANNEL_ID".into()
}
fn default_bot_token_env() -> String {
    "SLACK_BOT_TOKEN".into()
}
fn default_app_token_env() -> String {
    "SLACK_APP_TOKEN".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeConfig {
    /// How many recent messages to keep in Slack chat context.
    #[serde(default = "default_max_context_messages")]
    pub max_context_messages: u32,
    /// `SQLite` path for last-N memory + sources/corpus (relative to process cwd unless absolute).
    #[serde(default = "default_state_db_path")]
    pub state_db_path: String,
    /// Official `LinkedIn` data export directory or zip (relative to cwd unless absolute).
    #[serde(default = "default_linkedin_export_dir")]
    pub linkedin_export_dir: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_context_messages: default_max_context_messages(),
            state_db_path: default_state_db_path(),
            linkedin_export_dir: default_linkedin_export_dir(),
        }
    }
}

const fn default_max_context_messages() -> u32 {
    20
}
fn default_state_db_path() -> String {
    "../sql/runtime.db".into()
}
fn default_linkedin_export_dir() -> String {
    "../linkedin-export".into()
}

/// Failover routes: entries are `provider:model` (model may contain colons).
#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    /// Freeform Slack replies. TOML key `freeform_route` (alias `chat_chain`).
    #[serde(default = "default_freeform_route", alias = "chat_chain")]
    pub freeform_route: Vec<String>,
    /// Research / candidate pack before draft writer. TOML `load_route`.
    #[serde(default = "default_load_route")]
    pub load_route: Vec<String>,
    /// Grounded `LinkedIn` draft writer. TOML key `draft_route` (alias `draft_chain`).
    #[serde(default = "default_draft_route", alias = "draft_chain")]
    pub draft_route: Vec<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            freeform_route: default_freeform_route(),
            load_route: default_load_route(),
            draft_route: default_draft_route(),
        }
    }
}

fn default_freeform_route() -> Vec<String> {
    vec!["ollama:llama3.1:8b".into(), "ollama:gemma3:4b".into()]
}

fn default_load_route() -> Vec<String> {
    vec!["ollama:llama3.1:8b".into(), "ollama:qwen3.5:9b".into()]
}

fn default_draft_route() -> Vec<String> {
    vec![
        "ollama:llama3.1:8b".into(),
        "ollama:qwen3.5:9b".into(),
        "ollama:gemma4:12b".into(),
    ]
}

/// Company-page publish settings. Secrets stay in `.linkedin` / `.env`.
#[derive(Debug, Clone, Deserialize)]
pub struct LinkedInConfig {
    /// `playground` (default) or `production`. Override with `ITCY_LINKEDIN_PUBLISH_MODE`.
    #[serde(default = "default_publish_mode")]
    pub publish_mode: String,
    /// JSON-RPC MCP URL. Override with `ITCY_LINKEDIN_MCP_URL`.
    #[serde(default = "default_linkedin_mcp_url")]
    pub mcp_url: String,
}

impl Default for LinkedInConfig {
    fn default() -> Self {
        Self {
            publish_mode: default_publish_mode(),
            mcp_url: default_linkedin_mcp_url(),
        }
    }
}

fn default_publish_mode() -> String {
    "playground".into()
}

fn default_linkedin_mcp_url() -> String {
    "http://127.0.0.1:4780/mcp".into()
}

/// X ship settings. Secrets stay in `.twitter` / `.env`, not here.
#[derive(Debug, Clone, Deserialize)]
pub struct XConfig {
    /// `playground` (default) or `production`. Override with `ITCY_X_PUBLISH_MODE`.
    #[serde(default = "default_publish_mode")]
    pub publish_mode: String,
}

impl Default for XConfig {
    fn default() -> Self {
        Self {
            publish_mode: default_publish_mode(),
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("read config: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse config: {0}")]
    Parse(#[from] toml::de::Error),
}

impl Config {
    /// Reads and parses a TOML config file.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] when the TOML file is missing or invalid.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path)?;
        Ok(toml::from_str(&raw)?)
    }
}

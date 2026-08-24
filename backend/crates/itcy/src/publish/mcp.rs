// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Local HTTP MCP client for [vahabcore/linkedin-mcp-server](https://github.com/vahabcore/linkedin-mcp-server).
//!
//! JSON-RPC `tools/call` against `http://127.0.0.1:4780/mcp` (override with
//! `ITCY_LINKEDIN_MCP_URL` / `[linkedin].mcp_url`).

use super::{PublishError, PublishMode, PublishRequest, PublishResult, Publisher};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

const DEFAULT_MCP_URL: &str = "http://127.0.0.1:4780/mcp";

static RPC_ID: AtomicU64 = AtomicU64::new(1);

/// Loopback MCP URL for `LinkedIn` tools.
#[must_use]
pub fn resolve_mcp_url() -> String {
    std::env::var("ITCY_LINKEDIN_MCP_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(read_mcp_url_from_config)
        .unwrap_or_else(|| DEFAULT_MCP_URL.to_string())
}

/// HTTP client for all vahabcore `LinkedIn` MCP tools.
#[derive(Clone)]
pub struct LinkedInMcpClient {
    url: String,
    http: reqwest::Client,
}

impl LinkedInMcpClient {
    /// Builds from env / config / default loopback URL.
    #[must_use]
    pub fn new() -> Self {
        Self {
            url: resolve_mcp_url(),
            http: reqwest::Client::new(),
        }
    }

    /// Endpoint this client calls.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Low-level `tools/call`.
    ///
    /// # Errors
    ///
    /// Returns [`PublishError`] when HTTP, JSON-RPC, or the tool reports failure.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, PublishError> {
        let id = RPC_ID.fetch_add(1, Ordering::Relaxed);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        });
        let res = self
            .http
            .post(&self.url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| PublishError::Http(e.to_string()))?;
        let status = res.status();
        let text = res
            .text()
            .await
            .map_err(|e| PublishError::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(PublishError::Http(format!("{status}: {text}")));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| PublishError::Http(format!("mcp json: {e}")))?;
        if let Some(msg) = mcp_rpc_error(&v) {
            return Err(PublishError::Http(msg));
        }
        if mcp_tool_is_error(&v) {
            return Err(PublishError::Http(mcp_tool_text(&v)));
        }
        Ok(mcp_tool_text(&v))
    }

    /// `create_text_post`
    ///
    /// # Errors
    ///
    /// Tool or transport failure.
    pub async fn create_text_post(&self, content: &str) -> Result<String, PublishError> {
        self.call_tool("create_text_post", json!({ "content": content }))
            .await
    }

    /// `create_link_post`
    ///
    /// # Errors
    ///
    /// Tool or transport failure.
    pub async fn create_link_post(
        &self,
        content: &str,
        url: &str,
        title: &str,
    ) -> Result<String, PublishError> {
        self.call_tool(
            "create_link_post",
            json!({ "content": content, "url": url, "title": title }),
        )
        .await
    }

    /// `create_image_post`
    ///
    /// # Errors
    ///
    /// Tool or transport failure.
    pub async fn create_image_post(
        &self,
        content: &str,
        image_url: &str,
    ) -> Result<String, PublishError> {
        self.call_tool(
            "create_image_post",
            json!({ "content": content, "image_url": image_url }),
        )
        .await
    }

    /// `create_poll_post`
    ///
    /// # Errors
    ///
    /// Tool or transport failure.
    pub async fn create_poll_post(
        &self,
        question: &str,
        options: &[String],
        duration: &str,
    ) -> Result<String, PublishError> {
        self.call_tool(
            "create_poll_post",
            json!({
                "question": question,
                "options": options,
                "duration": duration
            }),
        )
        .await
    }

    /// `delete_post`
    ///
    /// # Errors
    ///
    /// Tool or transport failure.
    pub async fn delete_post(&self, post_urn: &str) -> Result<String, PublishError> {
        self.call_tool("delete_post", json!({ "post_urn": post_urn }))
            .await
    }

    /// `get_user_profile`
    ///
    /// # Errors
    ///
    /// Tool or transport failure.
    pub async fn get_user_profile(&self) -> Result<String, PublishError> {
        self.call_tool("get_user_profile", json!({})).await
    }

    /// `get_post_stats`
    ///
    /// # Errors
    ///
    /// Tool or transport failure.
    pub async fn get_post_stats(&self, post_urn: &str) -> Result<String, PublishError> {
        self.call_tool("get_post_stats", json!({ "post_urn": post_urn }))
            .await
    }

    /// `react_to_post`
    ///
    /// # Errors
    ///
    /// Tool or transport failure.
    pub async fn react_to_post(
        &self,
        post_urn: &str,
        reaction_type: &str,
    ) -> Result<String, PublishError> {
        self.call_tool(
            "react_to_post",
            json!({ "post_urn": post_urn, "reaction_type": reaction_type }),
        )
        .await
    }

    /// `remove_reaction`
    ///
    /// # Errors
    ///
    /// Tool or transport failure.
    pub async fn remove_reaction(&self, post_urn: &str) -> Result<String, PublishError> {
        self.call_tool("remove_reaction", json!({ "post_urn": post_urn }))
            .await
    }

    /// `comment_on_post`
    ///
    /// # Errors
    ///
    /// Tool or transport failure.
    pub async fn comment_on_post(
        &self,
        post_urn: &str,
        comment_text: &str,
    ) -> Result<String, PublishError> {
        self.call_tool(
            "comment_on_post",
            json!({ "post_urn": post_urn, "comment_text": comment_text }),
        )
        .await
    }

    /// `delete_comment`
    ///
    /// # Errors
    ///
    /// Tool or transport failure.
    pub async fn delete_comment(
        &self,
        post_urn: &str,
        comment_id: &str,
    ) -> Result<String, PublishError> {
        self.call_tool(
            "delete_comment",
            json!({ "post_urn": post_urn, "comment_id": comment_id }),
        )
        .await
    }

    /// `reply_to_comment`
    ///
    /// # Errors
    ///
    /// Tool or transport failure.
    pub async fn reply_to_comment(
        &self,
        post_urn: &str,
        parent_comment_urn: &str,
        reply_text: &str,
    ) -> Result<String, PublishError> {
        self.call_tool(
            "reply_to_comment",
            json!({
                "post_urn": post_urn,
                "parent_comment_urn": parent_comment_urn,
                "reply_text": reply_text
            }),
        )
        .await
    }
}

impl Default for LinkedInMcpClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Production publisher that ships body text via MCP `create_text_post`.
pub struct McpLinkedInPublisher {
    client: LinkedInMcpClient,
}

impl McpLinkedInPublisher {
    /// Builds a publisher using the shared MCP URL resolution.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: LinkedInMcpClient::new(),
        }
    }
}

impl Default for McpLinkedInPublisher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Publisher for McpLinkedInPublisher {
    fn mode(&self) -> PublishMode {
        PublishMode::Production
    }

    async fn publish_company_post(
        &self,
        request: &PublishRequest,
    ) -> Result<PublishResult, PublishError> {
        let body = request.body.trim();
        if body.is_empty() {
            return Err(PublishError::Other("empty post body".into()));
        }
        let tool_text = self.client.create_text_post(body).await?;
        let urn = extract_post_id(&tool_text);
        let url = urn
            .as_ref()
            .map(|u| format!("https://www.linkedin.com/feed/update/{u}"));
        Ok(PublishResult {
            mode: PublishMode::Production,
            linkedin_urn: urn,
            linkedin_url: url,
            detail: format!("mcp ship ok {}", truncate_for_log(&tool_text, 200)),
        })
    }
}

/// Activity feed URN for MCP tools (`urn:li:activity:…`).
#[must_use]
pub fn activity_post_urn(activity_id: &str) -> String {
    format!("urn:li:activity:{activity_id}")
}

/// Parent comment URN for `reply_to_comment`.
#[must_use]
pub fn parent_comment_urn(activity_id: &str, comment_id: &str) -> String {
    format!("urn:li:comment:(urn:li:activity:{activity_id},{comment_id})")
}

fn mcp_rpc_error(v: &Value) -> Option<String> {
    v.get("error").map(ToString::to_string)
}

fn mcp_tool_is_error(v: &Value) -> bool {
    v.pointer("/result/isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn mcp_tool_text(v: &Value) -> String {
    v.pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn extract_post_id(tool_text: &str) -> Option<String> {
    let marker = "Post ID:";
    let rest = tool_text.split(marker).nth(1)?;
    let id = rest.split_whitespace().next()?.trim().to_string();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn truncate_for_log(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}

/// Loopback MCP URL helpers and tool client.

fn read_mcp_url_from_config() -> Option<String> {
    for path in super::ship::config_toml_candidates() {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(v) = parse_mcp_url_toml(&raw) {
            return Some(v);
        }
    }
    None
}

fn parse_mcp_url_toml(raw: &str) -> Option<String> {
    let mut in_section = false;
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_section = line == "[linkedin]";
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(rest) = line.strip_prefix("mcp_url") {
            let rest = rest.trim().trim_start_matches('=').trim();
            let v = rest.trim_matches('"').trim_matches('\'').trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_post_id_from_tool_text() {
        assert_eq!(
            extract_post_id("Published text post to LinkedIn. Post ID: urn:li:share:1"),
            Some("urn:li:share:1".into())
        );
        assert_eq!(extract_post_id("no id here"), None);
    }

    #[test]
    fn parent_comment_urn_shape() {
        assert_eq!(
            parent_comment_urn("111", "222"),
            "urn:li:comment:(urn:li:activity:111,222)"
        );
        assert_eq!(activity_post_urn("111"), "urn:li:activity:111");
    }

    #[test]
    fn parse_mcp_url_linkedin_section() {
        let raw =
            "[linkedin]\npublish_mode = \"production\"\nmcp_url = \"http://127.0.0.1:4780/mcp\"\n";
        assert_eq!(
            parse_mcp_url_toml(raw).as_deref(),
            Some("http://127.0.0.1:4780/mcp")
        );
        assert_eq!(
            parse_mcp_url_toml("[x]\nmcp_url = \"http://127.0.0.1:4780/mcp\"\n").as_deref(),
            None
        );
    }

    #[test]
    fn mcp_tool_error_flag() {
        let v = serde_json::json!({
            "result": { "isError": true, "content": [{"type":"text","text":"Failed"}] }
        });
        assert!(mcp_tool_is_error(&v));
        assert_eq!(mcp_tool_text(&v), "Failed");
    }
}

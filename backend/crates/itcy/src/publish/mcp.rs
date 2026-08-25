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
        // Prefer ship_company_post which already strips; keep a safety net here.
        let body = linkedin_text_for_api(&request.body);
        if body.is_empty() {
            return Err(PublishError::Other("empty post body".into()));
        }
        let tool_text = self.client.create_text_post(&body).await?;
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

/// Drop Slack operator chrome from a draft/post body before `LinkedIn` ship.
///
/// Keeps prose, the bare `https://` cite line, and the trailing `Written by AI` disclosure.
/// Strips `Draft ID` / `Post ID` headers and the `Link:` / numbered-option block.
#[must_use]
pub fn linkedin_text_for_api(body: &str) -> String {
    let mut prose: Vec<&str> = Vec::new();
    let mut disclosure: Vec<&str> = Vec::new();
    let mut in_link_chrome = false;
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("Written by AI") {
            in_link_chrome = false;
            disclosure.push(line);
            continue;
        }
        if is_linkedin_link_chrome_start(t) {
            in_link_chrome = true;
            continue;
        }
        if in_link_chrome {
            continue;
        }
        if is_linkedin_id_header(t) {
            continue;
        }
        prose.push(line);
    }
    while prose.first().is_some_and(|l| l.trim().is_empty()) {
        prose.remove(0);
    }
    while prose.last().is_some_and(|l| l.trim().is_empty()) {
        prose.pop();
    }
    let mut out = crate::llm::sanitize_itcy_text(&prose.join("\n"));
    if !disclosure.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&disclosure.join("\n"));
    }
    out.trim().to_string()
}

fn is_linkedin_id_header(t: &str) -> bool {
    t.starts_with("Draft ID:") || t.starts_with("Post ID:")
}

fn is_linkedin_link_chrome_start(t: &str) -> bool {
    t.starts_with("Link:")
        || t.starts_with("0 = no link")
        || t.starts_with("Sources:")
        || t.starts_with("Sources used:")
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
    fn linkedin_text_keeps_prose_url_disclosure_strips_chrome() {
        let body = "\
Draft ID: DRAFT-20260824-000093\n\
\n\
The project, rangular, is a tiny experiment. 🦀\n\
\n\
https://github.com/Interchouette-ITC/rangular\n\
\n\
Link: 1\n\
0 = no link. /change_url DRAFT-20260824-000093 <0|1|2|3|url>\n\
1. https://github.com/Interchouette-ITC/rangular\n\
2. https://interchouette.net/news\n\
\n\
Written by AI - ITCy - model ollama/qwen3:8b - tokens in:3918 out:290\n";
        let out = linkedin_text_for_api(body);
        assert!(out.contains("rangular"));
        assert!(out.contains("🦀"));
        assert!(out.contains("https://github.com/Interchouette-ITC/rangular"));
        assert!(out.contains("Written by AI - ITCy - model ollama/qwen3:8b"));
        assert!(!out.contains("Draft ID:"));
        assert!(!out.contains("Link:"));
        assert!(!out.contains("0 = no link"));
        assert!(!out.contains("interchouette.net/news"));
        assert!(!out.contains("/change_url"));
    }

    #[test]
    fn linkedin_text_expands_shortcodes() {
        let out = linkedin_text_for_api("Hello :owl:\n\nhttps://example.com/a\n");
        assert!(out.contains('🦉'), "{out}");
        assert!(!out.contains(":owl:"));
    }

    #[test]
    fn linkedin_text_converts_spaced_hyphen_pauses() {
        let body = "\
Draft ID: DRAFT-1\n\
\n\
quirks - it is the cost\n\
\n\
https://example.com/a\n\
\n\
Link: 1\n\
Written by AI - ITCy - model ollama/qwen3:8b - tokens in:1 out:1\n";
        let out = linkedin_text_for_api(body);
        assert!(out.contains("quirks, it is the cost"), "{out}");
        assert!(!out.split("Written by AI").next().unwrap().contains(" - "));
        assert!(out.contains("Written by AI - ITCy - model"));
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

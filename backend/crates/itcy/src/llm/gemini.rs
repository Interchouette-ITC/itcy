// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Google Gemini generateContent client.

use crate::llm::client::{
    format_provider_http_error, LlmClient, LlmError, LlmMessage, LlmResponse, LlmRole, LlmToolDef,
    LlmUsage,
};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

/// Gemini Generative Language API client.
pub struct GeminiClient {
    http: Client,
    api_key: String,
    base_url: String,
}

impl GeminiClient {
    #[must_use]
    pub fn new(api_key: String, base_url: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            api_key,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    fn generate_url(&self, model: &str) -> String {
        format!(
            "{}/models/{model}:generateContent?key={}",
            self.base_url, self.api_key
        )
    }
}

fn build_body(messages: &[LlmMessage]) -> serde_json::Value {
    let mut system_parts: Vec<String> = Vec::new();
    let mut contents: Vec<serde_json::Value> = Vec::new();
    for m in messages {
        match m.role {
            LlmRole::System => system_parts.push(m.content.clone()),
            LlmRole::User => contents.push(serde_json::json!({
                "role": "user",
                "parts": [{ "text": m.content }],
            })),
            LlmRole::Assistant => contents.push(serde_json::json!({
                "role": "model",
                "parts": [{ "text": m.content }],
            })),
            LlmRole::Tool => contents.push(serde_json::json!({
                "role": "user",
                "parts": [{ "text": format!("Tool result: {}", m.content) }],
            })),
        }
    }
    let mut body = serde_json::json!({ "contents": contents });
    if !system_parts.is_empty() {
        body["systemInstruction"] = serde_json::json!({
            "parts": [{ "text": system_parts.join("\n") }],
        });
    }
    body
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: Option<GeminiContent>,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct GeminiContent {
    parts: Option<Vec<GeminiPart>>,
}

#[derive(Deserialize)]
struct GeminiPart {
    text: Option<String>,
}

#[derive(Deserialize)]
struct GeminiUsage {
    #[serde(default, rename = "promptTokenCount")]
    prompt_token_count: Option<u32>,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates_token_count: Option<u32>,
}

fn parse_response(text: &str) -> Result<LlmResponse, LlmError> {
    let parsed: GeminiResponse = serde_json::from_str(text)?;
    let candidate = parsed
        .candidates
        .and_then(|mut c| c.pop())
        .ok_or_else(|| LlmError::Provider("gemini: no candidates".into()))?;
    let parts = candidate.content.and_then(|c| c.parts).unwrap_or_default();
    let mut content = String::new();
    for part in parts {
        if let Some(t) = part.text {
            content.push_str(&t);
        }
    }
    let usage = parsed.usage_metadata.map(|u| LlmUsage {
        prompt_tokens: u.prompt_token_count.unwrap_or(0),
        completion_tokens: u.candidates_token_count.unwrap_or(0),
    });
    Ok(LlmResponse {
        message: LlmMessage {
            role: LlmRole::Assistant,
            content,
            tool_calls: None,
            tool_call_id: None,
        },
        finish_reason: candidate.finish_reason.unwrap_or_else(|| "STOP".into()),
        usage,
    })
}

#[async_trait]
impl LlmClient for GeminiClient {
    fn provider_id(&self) -> &'static str {
        "gemini"
    }

    async fn chat(
        &self,
        messages: &[LlmMessage],
        model: &str,
        tools: Option<&[LlmToolDef]>,
    ) -> Result<LlmResponse, LlmError> {
        if tools.is_some_and(|t| !t.is_empty()) {
            return Err(LlmError::Provider(
                "gemini: tool calling not wired in ITCy yet; try ollama or openai-compat route"
                    .into(),
            ));
        }
        let body = build_body(messages);
        let res = self
            .http
            .post(self.generate_url(model))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        let text = res.text().await?;
        if !status.is_success() {
            return Err(format_provider_http_error("gemini", status, &text));
        }
        parse_response(&text)
    }
}

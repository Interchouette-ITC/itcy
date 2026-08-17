// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! OpenAI-compatible chat client (Groq, `OpenRouter`, GitHub Models, Cerebras).

use crate::llm::client::{
    format_provider_http_error, sanitize_tool_arguments, LlmClient, LlmError, LlmMessage,
    LlmResponse, LlmRole, LlmToolCall, LlmToolDef, LlmUsage,
};
use async_trait::async_trait;
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;
use serde_json::Value as JsonValue;

const GITHUB_API_VERSION: &str = "2022-11-28";
const APP_TITLE: &str = "ITCy";
const APP_HTTP_REFERER: &str = "https://interchouette.net/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompatFlavor {
    Generic,
    OpenRouter,
    GitHub,
}

impl CompatFlavor {
    fn from_provider_id(provider_id: &str) -> Self {
        match provider_id {
            "openrouter" => Self::OpenRouter,
            "github" => Self::GitHub,
            _ => Self::Generic,
        }
    }
}

/// `OpenAI` `/chat/completions` compatible client.
pub struct OpenAiCompatibleClient {
    http: Client,
    provider_id: String,
    flavor: CompatFlavor,
    base_url: String,
    api_key: String,
}

impl OpenAiCompatibleClient {
    #[must_use]
    pub fn new(
        provider_id: impl Into<String>,
        api_key: String,
        base_url: impl Into<String>,
    ) -> Self {
        let provider_id = provider_id.into();
        let flavor = CompatFlavor::from_provider_id(&provider_id);
        Self {
            http: Client::new(),
            flavor,
            provider_id,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
        }
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn apply_headers(&self, builder: RequestBuilder) -> RequestBuilder {
        let builder = match self.flavor {
            CompatFlavor::OpenRouter => builder
                .header("HTTP-Referer", APP_HTTP_REFERER)
                .header("X-Title", APP_TITLE),
            CompatFlavor::GitHub => builder
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", GITHUB_API_VERSION),
            CompatFlavor::Generic => builder,
        };
        builder
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
    }
}

fn tools_json(tools: &[LlmToolDef]) -> Vec<JsonValue> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        })
        .collect()
}

fn build_messages(messages: &[LlmMessage]) -> Vec<JsonValue> {
    messages
        .iter()
        .map(|m| {
            let mut obj = serde_json::json!({
                "role": m.role.as_str(),
                "content": m.content,
            });
            if let Some(ref id) = m.tool_call_id {
                obj["tool_call_id"] = JsonValue::String(id.clone());
            }
            if let Some(ref calls) = m.tool_calls {
                let arr: Vec<JsonValue> = calls
                    .iter()
                    .map(|t| {
                        let args = sanitize_tool_arguments(&t.arguments);
                        serde_json::json!({
                            "id": t.id,
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "arguments": args,
                            }
                        })
                    })
                    .collect();
                obj["tool_calls"] = JsonValue::Array(arr);
            }
            obj
        })
        .collect()
}

fn build_body(
    messages: &[LlmMessage],
    model: &str,
    tools: Option<&[LlmToolDef]>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": build_messages(messages),
    });
    if let Some(tools) = tools {
        if !tools.is_empty() {
            body["tools"] = JsonValue::Array(tools_json(tools));
        }
    }
    body
}

#[derive(Deserialize)]
struct OpenAiUsage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct OpenAiFunction {
    name: String,
    #[serde(default)]
    arguments: String,
}

#[derive(Deserialize)]
struct OpenAiToolCallWire {
    id: String,
    function: OpenAiFunction,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    role: String,
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCallWire>>,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

fn map_usage(usage: Option<OpenAiUsage>) -> Option<LlmUsage> {
    usage.and_then(|u| {
        Some(LlmUsage {
            prompt_tokens: u.prompt_tokens?,
            completion_tokens: u.completion_tokens.unwrap_or(0),
        })
    })
}

fn parse_response(provider_id: &str, text: &str) -> Result<LlmResponse, LlmError> {
    let parsed: OpenAiResponse = serde_json::from_str(text)?;
    let choice = parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| LlmError::Provider(format!("{provider_id}: no choices in response")))?;
    let role = match choice.message.role.as_str() {
        "system" => LlmRole::System,
        "user" => LlmRole::User,
        "tool" => LlmRole::Tool,
        _ => LlmRole::Assistant,
    };
    let tool_calls = choice.message.tool_calls.map(|calls| {
        calls
            .into_iter()
            .map(|c| LlmToolCall {
                id: c.id,
                name: c.function.name,
                arguments: sanitize_tool_arguments(&c.function.arguments),
            })
            .collect()
    });
    Ok(LlmResponse {
        message: LlmMessage {
            role,
            content: choice.message.content.unwrap_or_default(),
            tool_calls,
            tool_call_id: None,
        },
        finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".into()),
        usage: map_usage(parsed.usage),
    })
}

#[async_trait]
impl LlmClient for OpenAiCompatibleClient {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    async fn chat(
        &self,
        messages: &[LlmMessage],
        model: &str,
        tools: Option<&[LlmToolDef]>,
    ) -> Result<LlmResponse, LlmError> {
        let body = build_body(messages, model, tools);
        let res = self
            .apply_headers(self.http.post(self.chat_url()))
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        let text = res.text().await?;
        if !status.is_success() {
            return Err(format_provider_http_error(&self.provider_id, status, &text));
        }
        parse_response(&self.provider_id, &text)
    }
}

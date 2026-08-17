// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! LLM client trait, messages, usage, and errors.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use thiserror::Error;

/// Role of a chat message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmRole {
    System,
    User,
    Assistant,
    /// Tool result turn (`OpenAI` / Ollama `role: tool`).
    Tool,
}

impl LlmRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

/// Definition of a tool (function) the model can call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmToolDef {
    pub name: String,
    pub description: String,
    pub parameters: JsonValue,
}

/// One tool call requested by the assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmToolCall {
    pub id: String,
    pub name: String,
    /// JSON arguments as a string (normalized for all providers).
    pub arguments: String,
}

/// One chat message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: String,
    pub tool_calls: Option<Vec<LlmToolCall>>,
    pub tool_call_id: Option<String>,
}

impl LlmMessage {
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::System,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::Assistant,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[must_use]
    pub fn tool_result(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self {
            role: LlmRole::Tool,
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// Ensures tool argument JSON is valid before echoing to providers.
#[must_use]
pub fn sanitize_tool_arguments(arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return "{}".into();
    }
    serde_json::from_str::<JsonValue>(trimmed).map_or_else(
        |_| "{}".into(),
        |v| serde_json::to_string(&v).unwrap_or_else(|_| "{}".into()),
    )
}

/// Token usage for one completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LlmUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// Provider chat response.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub message: LlmMessage,
    pub finish_reason: String,
    pub usage: Option<LlmUsage>,
}

/// Winning provider/model + tokens (stored on the draft for BAT / publications).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionTrace {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

impl CompletionTrace {
    #[must_use]
    pub fn from_response(provider: &str, model: &str, response: &LlmResponse) -> Self {
        let (prompt_tokens, completion_tokens) = response
            .usage
            .map_or((0, 0), |u| (u.prompt_tokens, u.completion_tokens));
        Self {
            provider: provider.to_string(),
            model: model.to_string(),
            prompt_tokens,
            completion_tokens,
        }
    }

    /// Accumulates usage across tool-loop rounds.
    #[must_use]
    pub fn accumulate(mut self, other: &Self) -> Self {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.provider.clone_from(&other.provider);
        self.model.clone_from(&other.model);
        self
    }

    #[must_use]
    pub fn model_label(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

/// Classified error kind for failover decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmErrorKind {
    Auth,
    RateLimit,
    Timeout,
    Network,
    Provider,
    Json,
}

/// LLM client / transport error.
#[derive(Debug, Error)]
pub enum LlmError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Provider error: {0}")]
    Provider(String),
    #[error("tool provider: {0}")]
    ToolProvider(String),
    #[error("No LLM providers configured")]
    NoProviders,
    #[error("All providers failed: {0}")]
    AllFailed(String),
}

impl LlmError {
    #[must_use]
    pub fn kind(&self) -> LlmErrorKind {
        match self {
            Self::Request(e) => {
                if e.is_timeout() {
                    LlmErrorKind::Timeout
                } else if e.is_connect() || e.is_request() {
                    LlmErrorKind::Network
                } else {
                    LlmErrorKind::Provider
                }
            }
            Self::Json(_) => LlmErrorKind::Json,
            Self::NoProviders | Self::AllFailed(_) | Self::ToolProvider(_) => {
                LlmErrorKind::Provider
            }
            Self::Provider(msg) => classify_provider_message(msg),
        }
    }

    /// True when the router should try the next candidate.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind(),
            LlmErrorKind::RateLimit | LlmErrorKind::Timeout | LlmErrorKind::Network
        )
    }
}

fn classify_provider_message(msg: &str) -> LlmErrorKind {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("invalid api key")
        || lower.contains("authentication")
    {
        LlmErrorKind::Auth
    } else if lower.contains("429") || lower.contains("rate limit") || lower.contains("too many") {
        LlmErrorKind::RateLimit
    } else if lower.contains("timeout") || lower.contains("timed out") {
        LlmErrorKind::Timeout
    } else if lower.contains("connection") || lower.contains("network") {
        LlmErrorKind::Network
    } else {
        LlmErrorKind::Provider
    }
}

/// Formats an HTTP failure into a provider error.
#[must_use]
pub fn format_provider_http_error(
    provider_id: &str,
    status: reqwest::StatusCode,
    body: &str,
) -> LlmError {
    let snippet: String = body.chars().take(400).collect();
    LlmError::Provider(format!("{provider_id}: HTTP {status}: {snippet}"))
}

/// Async chat client for one provider.
#[async_trait]
pub trait LlmClient: Send + Sync {
    fn provider_id(&self) -> &str;

    async fn chat(
        &self,
        messages: &[LlmMessage],
        model: &str,
        tools: Option<&[LlmToolDef]>,
    ) -> Result<LlmResponse, LlmError>;

    /// Pin model weights in provider memory (Ollama). Default: no-op.
    async fn warm_model(&self, _model: &str) -> Result<(), LlmError> {
        Ok(())
    }

    /// Drop every resident model (Ollama `/api/ps` + `keep_alive: 0`).
    ///
    /// Default: no-op.
    async fn unload_resident_models(&self) -> Result<(), LlmError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_kinds() {
        assert!(LlmError::Provider("429 rate limit".into()).is_retryable());
        assert!(LlmError::Provider("request timed out".into()).is_retryable());
        assert!(!LlmError::Provider("401 unauthorized".into()).is_retryable());
        assert!(!LlmError::Provider("invalid model".into()).is_retryable());
    }

    #[test]
    fn trace_from_response() {
        let response = LlmResponse {
            message: LlmMessage::assistant("hi"),
            finish_reason: "stop".into(),
            usage: Some(LlmUsage {
                prompt_tokens: 10,
                completion_tokens: 4,
            }),
        };
        let trace = CompletionTrace::from_response("groq", "llama-3.3-70b-versatile", &response);
        assert_eq!(trace.model_label(), "groq/llama-3.3-70b-versatile");
        assert_eq!(trace.prompt_tokens, 10);
        assert_eq!(trace.completion_tokens, 4);
    }

    #[test]
    fn sanitize_bad_json() {
        assert_eq!(sanitize_tool_arguments(""), "{}");
        assert_eq!(sanitize_tool_arguments("not-json"), "{}");
        assert_eq!(sanitize_tool_arguments(r#"{"url":"x"}"#), r#"{"url":"x"}"#);
    }
}

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Ollama local chat client (including OpenAI-style tools).

use crate::llm::client::{
    format_provider_http_error, sanitize_tool_arguments, LlmClient, LlmError, LlmMessage,
    LlmResponse, LlmRole, LlmToolCall, LlmToolDef, LlmUsage,
};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::time::Duration;
use tracing::{info, warn};

/// Max wait for boot warm `/api/generate` (load + pin). Past this, boot fails hard.
/// Cold load of a multi-GB chat model can exceed 2m on a busy host.
const WARM_TIMEOUT: Duration = Duration::from_mins(5);

/// How long Ollama keeps **chat** weights resident after a request.
///
/// Default `-1` = forever (`ITCy` warms chat at boot and never unloads).
/// Override with `OLLAMA_KEEP_ALIVE` (`-1`, `24h`, `7d`, `365d`, or seconds).
#[must_use]
pub fn keep_alive_json() -> JsonValue {
    parse_keep_alive(
        std::env::var("OLLAMA_KEEP_ALIVE").ok().as_deref(),
        JsonValue::from(-1),
    )
}

/// How long Ollama keeps the **embed** model resident after `/api/embed`.
///
/// Default `0` = unload when the request ends so it never sits next to chat in VRAM.
/// Override with `OLLAMA_EMBED_KEEP_ALIVE` (same forms as [`keep_alive_json`]).
#[must_use]
pub fn embed_keep_alive_json() -> JsonValue {
    parse_keep_alive(
        std::env::var("OLLAMA_EMBED_KEEP_ALIVE").ok().as_deref(),
        JsonValue::from(0),
    )
}

fn parse_keep_alive(raw: Option<&str>, default: JsonValue) -> JsonValue {
    match raw {
        None => default,
        Some(s) => {
            let s = s.trim();
            if s.is_empty() {
                return default;
            }
            if let Ok(n) = s.parse::<i64>() {
                return JsonValue::from(n);
            }
            JsonValue::String(s.to_string())
        }
    }
}

/// Chat context length. Ollama's default is 4096, which cannot hold a tool loop
/// (corpus + Brave SERP). Override with `OLLAMA_NUM_CTX`.
#[must_use]
pub fn num_ctx() -> u32 {
    parse_num_ctx(std::env::var("OLLAMA_NUM_CTX").ok().as_deref())
}

fn parse_num_ctx(raw: Option<&str>) -> u32 {
    const DEFAULT: u32 = 16_384;
    raw.and_then(|s| s.trim().parse().ok())
        .filter(|&n| n >= 2048)
        .unwrap_or(DEFAULT)
}

fn options_json() -> JsonValue {
    serde_json::json!({ "num_ctx": num_ctx() })
}

/// Ollama `/api/chat` client.
pub struct OllamaClient {
    http: Client,
    base_url: String,
}

impl OllamaClient {
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    fn chat_url(&self) -> String {
        format!("{}/api/chat", self.base_url)
    }

    fn generate_url(&self) -> String {
        format!("{}/api/generate", self.base_url)
    }

    fn ps_url(&self) -> String {
        format!("{}/api/ps", self.base_url)
    }

    /// Unload every model currently resident in Ollama (`keep_alive: 0`).
    ///
    /// Used before boot warm so a stale Forever-pinned runner (or a dead CUDA
    /// context) does not fight the fresh load.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::Provider`] when `/api/ps` or an unload request fails.
    pub async fn unload_all(&self) -> Result<(), LlmError> {
        let res = self
            .http
            .get(self.ps_url())
            .send()
            .await
            .map_err(|e| LlmError::Provider(format!("ollama ps: {e}")))?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format_provider_http_error("ollama ps", status, &text));
        }
        let parsed: OllamaPsResponse =
            serde_json::from_str(&text).unwrap_or(OllamaPsResponse { models: Vec::new() });
        if parsed.models.is_empty() {
            info!("ollama: no resident models to unload");
            return Ok(());
        }
        for entry in &parsed.models {
            let name = entry.model_name();
            if name.is_empty() {
                continue;
            }
            self.unload_model(name).await?;
            info!(model = %name, "ollama: unloaded resident model");
        }
        Ok(())
    }

    async fn unload_model(&self, model: &str) -> Result<(), LlmError> {
        let body = serde_json::json!({
            "model": model,
            "prompt": "",
            "stream": false,
            "keep_alive": 0,
        });
        let res = self
            .http
            .post(self.generate_url())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Provider(format!("ollama unload {model}: {e}")))?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format_provider_http_error("ollama unload", status, &text));
        }
        Ok(())
    }

    /// Load model weights and pin with [`keep_alive_json`] (boot warm / no unload).
    ///
    /// Uses a non-empty prompt: Ollama often ACK empty-prompt generate without leaving
    /// the model resident in `/api/ps`.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::Provider`] when the generate request fails, is non-success,
    /// exceeds the warm timeout, or the model is still absent from `/api/ps` after warm.
    pub async fn preload(&self, model: &str) -> Result<(), LlmError> {
        let body = serde_json::json!({
            "model": model,
            "prompt": "warm",
            "stream": false,
            "keep_alive": keep_alive_json(),
            "options": options_json(),
        });
        let send = self
            .http
            .post(self.generate_url())
            .header("Content-Type", "application/json")
            .json(&body)
            .send();
        let res = tokio::time::timeout(WARM_TIMEOUT, send)
            .await
            .map_err(|_| {
                LlmError::Provider(format!(
                    "ollama warm {model}: timed out after {}s",
                    WARM_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| LlmError::Provider(format!("ollama warm {model}: {e}")))?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format_provider_http_error("ollama warm", status, &text));
        }
        let resident = self.require_chat_on_gpu(model).await?;
        info!(
            model = %model,
            keep_alive = %keep_alive_json(),
            num_ctx = num_ctx(),
            size_bytes = resident.size,
            size_vram_bytes = resident.size_vram,
            "ollama: model warm"
        );
        Ok(())
    }

    /// Fail when `model` is missing from `/api/ps` or resident with `size_vram=0` (CPU-only).
    ///
    /// Used after chat warm so a CPU-only pin cannot pass as success.
    async fn require_chat_on_gpu(&self, model: &str) -> Result<OllamaPsModel, LlmError> {
        let res = self
            .http
            .get(self.ps_url())
            .send()
            .await
            .map_err(|e| LlmError::Provider(format!("ollama ps after warm: {e}")))?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format_provider_http_error(
                "ollama ps after warm",
                status,
                &text,
            ));
        }
        let parsed: OllamaPsResponse =
            serde_json::from_str(&text).unwrap_or(OllamaPsResponse { models: Vec::new() });
        let Some(resident) = lookup_resident(&parsed, model).cloned() else {
            return Err(LlmError::Provider(format!(
                "ollama warm {model}: HTTP ok but model not in /api/ps (not pinned)"
            )));
        };
        if resident.size_vram == 0 {
            return Err(LlmError::Provider(format!(
                "ollama warm {model}: resident but size_vram=0 (CPU-only); refusing boot"
            )));
        }
        Ok(resident)
    }

    async fn chat_once(
        &self,
        messages: &[LlmMessage],
        model: &str,
        tools: Option<&[LlmToolDef]>,
    ) -> Result<LlmResponse, LlmError> {
        let body = build_body(messages, model, tools);
        let res = self
            .http
            .post(self.chat_url())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        let text = res.text().await?;
        if !status.is_success() {
            return Err(format_provider_http_error("ollama", status, &text));
        }
        parse_response(&text)
    }
}

#[derive(Deserialize)]
struct OllamaPsResponse {
    #[serde(default)]
    models: Vec<OllamaPsModel>,
}

#[derive(Clone, Deserialize)]
struct OllamaPsModel {
    #[serde(default)]
    name: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    size_vram: u64,
}

impl OllamaPsModel {
    fn model_name(&self) -> &str {
        if self.name.is_empty() {
            &self.model
        } else {
            &self.name
        }
    }
}

fn lookup_resident<'a>(parsed: &'a OllamaPsResponse, model: &str) -> Option<&'a OllamaPsModel> {
    let want = model.trim().to_ascii_lowercase();
    let want_base = want.split(':').next().unwrap_or(&want);
    parsed.models.iter().find(|m| {
        let n = m.model_name().to_ascii_lowercase();
        n == want || n.starts_with(&format!("{want}:")) || n.starts_with(want_base)
    })
}

fn is_cuda_runner_death(err: &LlmError) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("model runner has unexpectedly stopped")
        || msg.contains("cuda error")
        || msg.contains("unspecified launch failure")
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
                        let args_value: JsonValue =
                            serde_json::from_str(&sanitize_tool_arguments(&t.arguments))
                                .unwrap_or_else(|_| JsonValue::Object(serde_json::Map::new()));
                        serde_json::json!({
                            "id": t.id,
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "arguments": args_value,
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
    // Qwen3.x defaults to thinking: answer lands in `message.thinking` while `content`
    // stays empty. ITCy needs visible answers for ResearchPack + LinkedIn posts.
    let mut body = serde_json::json!({
        "model": model,
        "messages": build_messages(messages),
        "stream": false,
        "think": false,
        "keep_alive": keep_alive_json(),
        "options": options_json(),
    });
    if let Some(tools) = tools {
        if !tools.is_empty() {
            body["tools"] = JsonValue::Array(tools_json(tools));
        }
    }
    body
}

#[derive(Deserialize)]
struct OllamaFunction {
    name: String,
    #[serde(default)]
    arguments: JsonValue,
}

#[derive(Deserialize)]
struct OllamaToolCallWire {
    #[serde(default)]
    id: Option<String>,
    function: OllamaFunction,
}

#[derive(Deserialize)]
struct OllamaMessage {
    role: String,
    #[serde(default)]
    content: String,
    /// Qwen3 / thinking models may put a monologue here while `content` stays empty.
    /// We never promote this to `content` (Slack vomit); draft/freeform fail on empty content.
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OllamaToolCallWire>>,
}

#[derive(Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
}

fn parse_response(text: &str) -> Result<LlmResponse, LlmError> {
    let parsed: OllamaResponse = serde_json::from_str(text)?;
    let role = match parsed.message.role.as_str() {
        "system" => LlmRole::System,
        "user" => LlmRole::User,
        "tool" => LlmRole::Tool,
        _ => LlmRole::Assistant,
    };
    let tool_calls: Option<Vec<LlmToolCall>> = parsed.message.tool_calls.map(|calls| {
        calls
            .into_iter()
            .map(|c| {
                let id =
                    c.id.filter(|s| !s.is_empty())
                        .unwrap_or_else(|| format!("call_{}", c.function.name));
                let arguments = match c.function.arguments {
                    JsonValue::String(s) => sanitize_tool_arguments(&s),
                    other => other.to_string(),
                };
                LlmToolCall {
                    id,
                    name: c.function.name,
                    arguments,
                }
            })
            .collect()
    });
    let usage = match (parsed.prompt_eval_count, parsed.eval_count) {
        (Some(prompt_tokens), Some(completion_tokens)) => Some(LlmUsage {
            prompt_tokens,
            completion_tokens,
        }),
        (Some(prompt_tokens), None) => Some(LlmUsage {
            prompt_tokens,
            completion_tokens: 0,
        }),
        _ => None,
    };
    let finish = match &tool_calls {
        Some(calls) if !calls.is_empty() => "tool_calls".into(),
        _ => parsed.done_reason.unwrap_or_else(|| "stop".into()),
    };
    // Qwen / thinking models often leave `content` empty and put a monologue in `thinking`.
    // Never promote thinking to the visible answer (that leaked into Slack as fake "drafts").
    let content = parsed.message.content;
    if content.trim().is_empty() && tool_calls.as_ref().is_none_or(std::vec::Vec::is_empty) {
        let thinking_chars = parsed
            .message
            .thinking
            .as_ref()
            .map_or(0, |t| t.trim().len());
        if thinking_chars > 0 {
            warn!(
                thinking_chars,
                "ollama: empty content with non-empty thinking; leaving content empty"
            );
        }
    }
    Ok(LlmResponse {
        message: LlmMessage {
            role,
            content,
            tool_calls,
            tool_call_id: None,
        },
        finish_reason: finish,
        usage,
    })
}

#[async_trait]
impl LlmClient for OllamaClient {
    fn provider_id(&self) -> &'static str {
        "ollama"
    }

    async fn chat(
        &self,
        messages: &[LlmMessage],
        model: &str,
        tools: Option<&[LlmToolDef]>,
    ) -> Result<LlmResponse, LlmError> {
        match self.chat_once(messages, model, tools).await {
            Ok(response) => Ok(response),
            Err(err) if is_cuda_runner_death(&err) => {
                warn!(
                    model = %model,
                    error = %err,
                    "ollama: CUDA/runner death; unload + rewarm then one retry"
                );
                if let Err(unload_err) = self.unload_all().await {
                    warn!(error = %unload_err, "ollama: unload after CUDA death failed");
                }
                if let Err(warm_err) = self.preload(model).await {
                    warn!(error = %warm_err, "ollama: rewarm after CUDA death failed");
                    return Err(err);
                }
                self.chat_once(messages, model, tools).await
            }
            Err(err) => Err(err),
        }
    }

    async fn warm_model(&self, model: &str) -> Result<(), LlmError> {
        self.preload(model).await
    }

    async fn unload_resident_models(&self) -> Result<(), LlmError> {
        self.unload_all().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_body_disables_think_so_content_is_visible() {
        let body = build_body(&[LlmMessage::user("hi")], "qwen3.5:4b", None);
        assert_eq!(body.get("think"), Some(&JsonValue::Bool(false)));
        assert_eq!(body.get("stream"), Some(&JsonValue::Bool(false)));
        assert_eq!(body.get("keep_alive"), Some(&JsonValue::from(-1)));
        assert_eq!(
            body.get("options")
                .and_then(|o| o.get("num_ctx"))
                .and_then(JsonValue::as_u64),
            Some(u64::from(num_ctx()))
        );
        assert_eq!(
            body.get("model").and_then(|v| v.as_str()),
            Some("qwen3.5:4b")
        );
    }

    #[test]
    fn chat_body_with_tools_still_disables_think() {
        let tools = [LlmToolDef {
            name: "corpus_search".into(),
            description: "search".into(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let body = build_body(&[LlmMessage::user("hi")], "qwen3.5:4b", Some(&tools));
        assert_eq!(body.get("think"), Some(&JsonValue::Bool(false)));
        assert_eq!(body.get("keep_alive"), Some(&JsonValue::from(-1)));
        assert!(body.get("tools").and_then(|t| t.as_array()).is_some());
        assert_eq!(
            body.get("options")
                .and_then(|o| o.get("num_ctx"))
                .and_then(JsonValue::as_u64),
            Some(u64::from(num_ctx()))
        );
    }

    #[test]
    fn num_ctx_defaults_to_16k_and_rejects_tiny_windows() {
        assert_eq!(parse_num_ctx(None), 16_384);
        assert_eq!(parse_num_ctx(Some("")), 16_384);
        assert_eq!(parse_num_ctx(Some("bogus")), 16_384);
        assert_eq!(parse_num_ctx(Some("1024")), 16_384);
        assert_eq!(parse_num_ctx(Some("32768")), 32_768);
        assert_eq!(parse_num_ctx(Some(" 16384 ")), 16_384);
        assert_eq!(parse_num_ctx(Some("12288")), 12_288);
    }

    #[test]
    fn keep_alive_parser_defaults_and_forms() {
        assert_eq!(
            parse_keep_alive(None, JsonValue::from(-1)),
            JsonValue::from(-1)
        );
        assert_eq!(
            parse_keep_alive(None, JsonValue::from(0)),
            JsonValue::from(0)
        );
        assert_eq!(
            parse_keep_alive(Some(""), JsonValue::from(0)),
            JsonValue::from(0)
        );
        assert_eq!(
            parse_keep_alive(Some("60"), JsonValue::from(0)),
            JsonValue::from(60)
        );
        assert_eq!(
            parse_keep_alive(Some("1m"), JsonValue::from(0)),
            JsonValue::String("1m".into())
        );
    }

    #[test]
    fn lookup_resident_requires_vram_for_gpu() {
        let parsed: OllamaPsResponse = serde_json::from_str(
            r#"{"models":[{"name":"qwen3:8b","size":7200000000,"size_vram":5100000000}]}"#,
        )
        .expect("ps json");
        let gpu = lookup_resident(&parsed, "qwen3:8b").expect("found");
        assert!(gpu.size_vram > 0);
        let cpu_only: OllamaPsResponse = serde_json::from_str(
            r#"{"models":[{"name":"qwen3:8b","size":10000000000,"size_vram":0}]}"#,
        )
        .expect("ps json");
        let cpu = lookup_resident(&cpu_only, "qwen3:8b").expect("found");
        assert_eq!(cpu.size_vram, 0);
        assert!(lookup_resident(&parsed, "missing").is_none());
    }

    #[test]
    fn cuda_runner_death_detection() {
        let err = LlmError::Provider(
            "ollama: HTTP 500: model runner has unexpectedly stopped: CUDA error".into(),
        );
        assert!(is_cuda_runner_death(&err));
        assert!(!is_cuda_runner_death(&LlmError::Provider(
            "invalid model".into()
        )));
    }
}

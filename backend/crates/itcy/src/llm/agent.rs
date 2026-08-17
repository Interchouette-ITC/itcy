// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Tool provider trait and chat-with-tools agent loop.

use crate::llm::client::{
    CompletionTrace, LlmClient, LlmError, LlmMessage, LlmResponse, LlmToolCall, LlmToolDef,
};
use crate::llm::prompt_dump::dump_llm_prompt;
use async_trait::async_trait;
use tracing::{info, warn};

/// Default max tool rounds for freeform / draft.
pub const DEFAULT_MAX_TOOL_ROUNDS: u32 = 5;

/// Provides tool definitions and execution for the LLM.
#[async_trait]
pub trait ToolProvider: Send + Sync {
    async fn list_tools(&self) -> Result<Vec<LlmToolDef>, LlmError>;
    async fn call_tool(&self, name: &str, arguments: &str) -> Result<String, LlmError>;
}

/// Runs chat with an optional tool loop on a single client+model.
/// `task` labels the prompt dump folder (`load` / `draft` / `freeform`).
///
/// # Errors
///
/// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
///
/// # Panics
///
/// Panics when a lock is poisoned or an invariant is violated.
pub async fn chat_with_tools(
    client: &dyn LlmClient,
    model: &str,
    messages: &[LlmMessage],
    tools_provider: Option<&dyn ToolProvider>,
    max_rounds: u32,
    task: &str,
) -> Result<(LlmResponse, CompletionTrace), LlmError> {
    let Some(provider) = tools_provider else {
        dump_llm_prompt(task, client.provider_id(), model, messages);
        let response = client.chat(messages, model, None).await?;
        reject_empty_assistant_content(&response, task)?;
        let trace = CompletionTrace::from_response(client.provider_id(), model, &response);
        return Ok((response, trace));
    };

    let tools = provider.list_tools().await?;
    if tools.is_empty() {
        dump_llm_prompt(task, client.provider_id(), model, messages);
        let response = client.chat(messages, model, None).await?;
        reject_empty_assistant_content(&response, task)?;
        let trace = CompletionTrace::from_response(client.provider_id(), model, &response);
        return Ok((response, trace));
    }

    let mut current: Vec<LlmMessage> = messages.to_vec();
    let mut acc_trace: Option<CompletionTrace> = None;
    let mut rounds: u32 = 0;

    loop {
        rounds += 1;
        dump_llm_prompt(task, client.provider_id(), model, &current);
        let response = client.chat(&current, model, Some(&tools)).await?;
        let round_trace = CompletionTrace::from_response(client.provider_id(), model, &response);
        log_round_usage(client, model, task, rounds, &round_trace, false);
        acc_trace = Some(match acc_trace {
            Some(prev) => prev.accumulate(&round_trace),
            None => round_trace,
        });

        let Some(ref tool_calls) = response.message.tool_calls else {
            reject_empty_assistant_content(&response, task)?;
            return Ok((response, acc_trace.expect("trace")));
        };
        if tool_calls.is_empty() {
            reject_empty_assistant_content(&response, task)?;
            return Ok((response, acc_trace.expect("trace")));
        }

        info!(
            provider = %client.provider_id(),
            model = %model,
            round = rounds,
            n_tools = tool_calls.len(),
            "llm: model requested tool calls"
        );

        current.push(LlmMessage {
            role: response.message.role,
            content: response.message.content.clone(),
            tool_calls: response.message.tool_calls.clone(),
            tool_call_id: None,
        });
        append_tool_results(client, model, provider, tool_calls, rounds, &mut current).await;

        if rounds >= max_rounds {
            warn!(
                rounds,
                "llm: tool round cap reached; final call WITHOUT tools"
            );
            dump_llm_prompt(task, client.provider_id(), model, &current);
            let response = client.chat(&current, model, None).await?;
            reject_empty_assistant_content(&response, task)?;
            let round_trace =
                CompletionTrace::from_response(client.provider_id(), model, &response);
            log_round_usage(client, model, task, rounds, &round_trace, true);
            let trace = acc_trace
                .unwrap_or_else(|| round_trace.clone())
                .accumulate(&round_trace);
            return Ok((response, trace));
        }
    }
}

/// Run each requested tool and append `role: tool` turns.
async fn append_tool_results(
    client: &dyn LlmClient,
    model: &str,
    provider: &dyn ToolProvider,
    tool_calls: &[LlmToolCall],
    round: u32,
    current: &mut Vec<LlmMessage>,
) {
    for tc in tool_calls {
        info!(
            provider = %client.provider_id(),
            model = %model,
            tool = %tc.name,
            round,
            arguments = %tc.arguments,
            "llm: executing tool"
        );
        let content = match provider.call_tool(&tc.name, &tc.arguments).await {
            Ok(s) => {
                info!(tool = %tc.name, round, result_len = s.len(), "llm: tool ok");
                s
            }
            Err(e) => {
                warn!(tool = %tc.name, round, error = %e, "llm: tool failed");
                format!("error: {e}")
            }
        };
        current.push(LlmMessage::tool_result(content, &tc.id));
    }
}

/// Log per-round token counts (not the accumulated total across the tool loop).
fn log_round_usage(
    client: &dyn LlmClient,
    model: &str,
    task: &str,
    round: u32,
    trace: &CompletionTrace,
    final_no_tools: bool,
) {
    info!(
        provider = %client.provider_id(),
        model = %model,
        task,
        round,
        prompt_tokens = trace.prompt_tokens,
        completion_tokens = trace.completion_tokens,
        final_no_tools,
        "llm: round usage"
    );
}

/// Refuse blank final answers for writer/chat tasks.
///
/// LOAD may return empty `content` after tools; rag recovers URLs from the session.
/// Visible answers for Qwen come from Ollama `think: false`, not a prose fallback nudge.
fn reject_empty_assistant_content(response: &LlmResponse, task: &str) -> Result<(), LlmError> {
    if task == "load" {
        return Ok(());
    }
    if response.message.content.trim().is_empty() {
        return Err(LlmError::Provider(format!(
            "{task}: model returned empty content (no visible answer)"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::client::{LlmRole, LlmToolCall, LlmUsage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct MockTools;

    #[async_trait]
    impl ToolProvider for MockTools {
        async fn list_tools(&self) -> Result<Vec<LlmToolDef>, LlmError> {
            Ok(vec![LlmToolDef {
                name: "corpus_search".into(),
                description: "search".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }),
            }])
        }

        async fn call_tool(&self, name: &str, _arguments: &str) -> Result<String, LlmError> {
            Ok(format!("{name}: hit from corpus"))
        }
    }

    struct ToolThenAnswer {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl LlmClient for ToolThenAnswer {
        fn provider_id(&self) -> &'static str {
            "mock"
        }

        async fn chat(
            &self,
            messages: &[LlmMessage],
            model: &str,
            _tools: Option<&[LlmToolDef]>,
        ) -> Result<LlmResponse, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                return Ok(LlmResponse {
                    message: LlmMessage {
                        role: LlmRole::Assistant,
                        content: String::new(),
                        tool_calls: Some(vec![LlmToolCall {
                            id: "c1".into(),
                            name: "corpus_search".into(),
                            arguments: r#"{"query":"rtk"}"#.into(),
                        }]),
                        tool_call_id: None,
                    },
                    finish_reason: "tool_calls".into(),
                    usage: Some(LlmUsage {
                        prompt_tokens: 5,
                        completion_tokens: 2,
                    }),
                });
            }
            let has_tool = messages.iter().any(|m| m.role == LlmRole::Tool);
            assert!(has_tool, "expected tool result in history");
            Ok(LlmResponse {
                message: LlmMessage::assistant(format!("draft via {model}")),
                finish_reason: "stop".into(),
                usage: Some(LlmUsage {
                    prompt_tokens: 8,
                    completion_tokens: 4,
                }),
            })
        }
    }

    #[tokio::test]
    async fn tool_loop_runs_then_answers() {
        let client = ToolThenAnswer {
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let (response, trace) = chat_with_tools(
            &client,
            "test-model",
            &[LlmMessage::user("draft about rtk")],
            Some(&MockTools),
            5,
            "draft",
        )
        .await
        .expect("loop");
        assert!(response.message.content.contains("draft via"));
        assert_eq!(trace.prompt_tokens, 13);
        assert_eq!(trace.completion_tokens, 6);
    }

    /// Empty content after tools must fail (no prose-nudge fallback). Fix is Ollama `think: false`.
    struct ToolThenEmptyStop {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl LlmClient for ToolThenEmptyStop {
        fn provider_id(&self) -> &'static str {
            "mock"
        }

        async fn chat(
            &self,
            _messages: &[LlmMessage],
            _model: &str,
            _tools: Option<&[LlmToolDef]>,
        ) -> Result<LlmResponse, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                return Ok(LlmResponse {
                    message: LlmMessage {
                        role: LlmRole::Assistant,
                        content: String::new(),
                        tool_calls: Some(vec![LlmToolCall {
                            id: "c1".into(),
                            name: "corpus_search".into(),
                            arguments: r#"{"query":"rtk"}"#.into(),
                        }]),
                        tool_call_id: None,
                    },
                    finish_reason: "tool_calls".into(),
                    usage: Some(LlmUsage {
                        prompt_tokens: 5,
                        completion_tokens: 2,
                    }),
                });
            }
            Ok(LlmResponse {
                message: LlmMessage {
                    role: LlmRole::Assistant,
                    content: String::new(),
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: "stop".into(),
                usage: Some(LlmUsage {
                    prompt_tokens: 8,
                    completion_tokens: 4,
                }),
            })
        }
    }

    #[tokio::test]
    async fn empty_content_after_tools_errors_without_prose_fallback() {
        let client = ToolThenEmptyStop {
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let err = chat_with_tools(
            &client,
            "test-model",
            &[LlmMessage::user("draft about rtk")],
            Some(&MockTools),
            5,
            "draft",
        )
        .await
        .expect_err("must not invent a prose fallback");
        let msg = err.to_string();
        assert!(msg.contains("empty content"));
        assert!(!msg.to_ascii_lowercase().contains("stronger model"));
        assert_eq!(client.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn empty_content_error_has_no_operator_advice() {
        let err = reject_empty_assistant_content(
            &LlmResponse {
                message: LlmMessage::assistant(""),
                finish_reason: "stop".into(),
                usage: None,
            },
            "draft",
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("empty content"));
        assert!(!msg.to_ascii_lowercase().contains("stronger model"));
        assert!(!msg.to_ascii_lowercase().contains("retry or"));
        assert!(!msg.to_ascii_lowercase().contains("refusing to post"));
    }
}

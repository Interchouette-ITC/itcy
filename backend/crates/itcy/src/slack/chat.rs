// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Model-backed runtime chat using last-N memory + failover router + tools.

use crate::llm::agent::ToolProvider;
use crate::llm::client::LlmMessage;
use crate::llm::clock::today_context_line;
use crate::llm::disclosure::{strip_trailing_disclosures, with_disclosure};
use crate::llm::router::{FailoverRouter, TaskKind};
use crate::llm::LlmError;
use crate::memory::StoredMessage;
use crate::slack::commands::normalize_user_text;

use crate::prompts::{FREEFORM_SYSTEM_CORE, WHO_IS_WHO};

fn freeform_system_prompt() -> String {
    format!(
        "{}\n\n{}\n\n{}",
        today_context_line(),
        WHO_IS_WHO,
        FREEFORM_SYSTEM_CORE
    )
}

/// Builds LLM messages from last-N history plus the new user turn.
#[must_use]
pub fn build_chat_messages(user_text: &str, history: &[StoredMessage]) -> Vec<LlmMessage> {
    let mut messages = vec![LlmMessage::system(freeform_system_prompt())];
    for msg in history {
        match msg.role.as_str() {
            // Drop prior footers so the model does not echo them into the next reply.
            "assistant" => {
                messages.push(LlmMessage::assistant(strip_trailing_disclosures(
                    &msg.content,
                )));
            }
            _ => messages.push(LlmMessage::user(&msg.content)),
        }
    }
    messages.push(LlmMessage::user(normalize_user_text(user_text)));
    messages
}

/// Runs the chat task through the failover router (with tools when provided).
///
/// # Errors
///
/// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
pub async fn build_runtime_reply(
    router: &FailoverRouter,
    user_text: &str,
    history: &[StoredMessage],
    tools: Option<&dyn ToolProvider>,
) -> Result<String, LlmError> {
    let messages = build_chat_messages(user_text, history);
    let (response, trace) = router
        .complete_with_tools(TaskKind::Freeform, &messages, tools, 8)
        .await?;
    Ok(with_disclosure(&response.message.content, &trace))
}

/// Fallback when no providers are registered or all fail.
#[must_use]
pub fn llm_unavailable_reply(err: &LlmError) -> String {
    format!(
        "ITCy could not complete a model-backed reply ({err}). \
Set at least one provider key in `.env`, or check ollama. \
Runtime commands `help` / `status` still work."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::client::{LlmClient, LlmResponse, LlmToolDef, LlmUsage};
    use crate::llm::router::{ChainCandidate, TaskChains};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Arc;

    struct OkClient;

    #[async_trait]
    impl LlmClient for OkClient {
        fn provider_id(&self) -> &'static str {
            "mock"
        }

        async fn chat(
            &self,
            _messages: &[LlmMessage],
            model: &str,
            _tools: Option<&[LlmToolDef]>,
        ) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                message: LlmMessage::assistant(format!("pong via {model}")),
                finish_reason: "stop".into(),
                usage: Some(LlmUsage {
                    prompt_tokens: 5,
                    completion_tokens: 2,
                }),
            })
        }
    }

    #[tokio::test]
    async fn freeform_includes_disclosure() {
        let mut clients: HashMap<String, Arc<dyn LlmClient>> = HashMap::new();
        clients.insert("mock".into(), Arc::new(OkClient));
        let chains = TaskChains::new().with_chain(
            TaskKind::Freeform,
            vec![ChainCandidate::new("mock", "test-model")],
        );
        let router = FailoverRouter::new(clients, chains);
        let reply = build_runtime_reply(&router, "ping", &[], None)
            .await
            .expect("reply");
        assert!(reply.contains("pong via test-model"));
        assert!(reply.contains("Written by AI - ITCy - model mock/test-model"));
        assert!(reply.contains("tokens in:5 out:2"));
    }

    #[test]
    fn history_maps_roles() {
        let history = vec![
            StoredMessage {
                role: "user".into(),
                content: "hi".into(),
            },
            StoredMessage {
                role: "assistant".into(),
                content: "hello".into(),
            },
        ];
        let msgs = build_chat_messages("next", &history);
        assert_eq!(msgs[0].role, crate::llm::client::LlmRole::System);
        assert!(msgs[0].content.contains("Today's date"));
        assert_eq!(msgs[1].content, "hi");
        assert_eq!(msgs[2].role, crate::llm::client::LlmRole::Assistant);
        assert_eq!(msgs[3].content, "next");
        assert!(msgs[0].content.contains("web_search"));
    }
}

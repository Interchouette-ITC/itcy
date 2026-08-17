// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Cross-provider failover router.

use crate::llm::agent::{chat_with_tools, ToolProvider, DEFAULT_MAX_TOOL_ROUNDS};
use crate::llm::client::{CompletionTrace, LlmClient, LlmError, LlmMessage, LlmResponse};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Task kind used to select a candidate route (providers + models).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskKind {
    /// Slack freeform replies (normal channel messages).
    Freeform,
    /// Draft research / candidate pack (`load` before writer).
    Load,
    /// LinkedIn-style grounded draft writer (`draft about X` in Slack).
    Draft,
}

impl TaskKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Freeform => "freeform",
            Self::Load => "load",
            Self::Draft => "draft",
        }
    }
}

/// One `(provider_id, model_id)` candidate in a task chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainCandidate {
    pub provider: String,
    pub model: String,
}

impl ChainCandidate {
    #[must_use]
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }

    /// `provider:model` (model may contain colons, e.g. `OpenRouter` ids).
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}:{}", self.provider, self.model)
    }

    /// Parses `provider:model` (model may contain colons, e.g. `OpenRouter` ids).
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        let (provider, model) = spec.split_once(':')?;
        let provider = provider.trim();
        let model = model.trim();
        if provider.is_empty() || model.is_empty() {
            return None;
        }
        Some(Self::new(provider, model))
    }
}

/// Formats a failover route as `provider:model, provider:model, ...`.
#[must_use]
pub fn format_route(chain: &[ChainCandidate]) -> String {
    if chain.is_empty() {
        return "(empty)".into();
    }
    chain
        .iter()
        .map(ChainCandidate::label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Ordered failover chains per task.
#[derive(Debug, Clone, Default)]
pub struct TaskChains {
    chains: HashMap<TaskKind, Vec<ChainCandidate>>,
}

impl TaskChains {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_chain(mut self, task: TaskKind, chain: Vec<ChainCandidate>) -> Self {
        self.chains.insert(task, chain);
        self
    }

    #[must_use]
    pub fn chain_for(&self, task: TaskKind) -> &[ChainCandidate] {
        self.chains.get(&task).map_or(&[][..], Vec::as_slice)
    }
}

/// Walks a task chain across providers until one succeeds.
pub struct FailoverRouter {
    clients: HashMap<String, Arc<dyn LlmClient>>,
    chains: TaskChains,
}

impl FailoverRouter {
    #[must_use]
    pub fn new(clients: HashMap<String, Arc<dyn LlmClient>>, chains: TaskChains) -> Self {
        Self { clients, chains }
    }

    #[must_use]
    pub fn provider_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.clients.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }

    #[must_use]
    pub fn has_providers(&self) -> bool {
        !self.clients.is_empty()
    }

    /// Failover route for a task (`provider:model, ...`).
    #[must_use]
    pub fn route(&self, task: TaskKind) -> String {
        format_route(self.chains.chain_for(task))
    }

    /// Preference order for a task (`provider:model, ...`).
    #[must_use]
    pub fn preference_chain(&self, task: TaskKind) -> String {
        self.route(task)
    }

    /// First route slot whose provider is in the live pool (primary / route head).
    #[must_use]
    pub fn route_head(&self, task: TaskKind) -> Option<&ChainCandidate> {
        self.chains
            .chain_for(task)
            .iter()
            .find(|c| self.clients.contains_key(&c.provider))
    }

    /// First preference whose provider is in the live pool (the balancer "chef").
    #[must_use]
    pub fn chef(&self, task: TaskKind) -> Option<&ChainCandidate> {
        self.route_head(task)
    }

    /// Route-head label, or `(none)` when no pooled provider matches.
    #[must_use]
    pub fn route_head_label(&self, task: TaskKind) -> String {
        self.route_head(task)
            .map_or_else(|| "(none)".into(), ChainCandidate::label)
    }

    /// Chef label, or `(none)` when the chain has no pooled provider.
    #[must_use]
    pub fn chef_label(&self, task: TaskKind) -> String {
        self.route_head_label(task)
    }

    /// Unique Ollama model ids currently on freeform / load / draft routes.
    /// With `FAST_DEV`, routes collapse to one model so this returns at most that one chat model.
    #[must_use]
    pub fn ollama_chat_models(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for task in [TaskKind::Freeform, TaskKind::Load, TaskKind::Draft] {
            for c in self.chains.chain_for(task) {
                if c.provider != "ollama" {
                    continue;
                }
                if !out.iter().any(|m| m == &c.model) {
                    out.push(c.model.clone());
                }
            }
        }
        out
    }

    /// Warm Ollama chat models used by live routes.
    /// Under `FAST_DEV`, routes share one model so only that id is warmed.
    /// No-op when Ollama is not on any live route.
    ///
    /// # Errors
    ///
    /// Returns the first [`LlmError`] from a failed warm (caller should abort boot).
    pub async fn warm_ollama_chat_models(&self) -> Result<(), LlmError> {
        let models = self.ollama_chat_models();
        if models.is_empty() {
            return Ok(());
        }
        let Some(client) = self.clients.get("ollama") else {
            return Ok(());
        };
        info!(
            models = %models.join(", "),
            "ollama: warming chat model(s)"
        );
        for model in &models {
            client.warm_model(model).await?;
        }
        Ok(())
    }

    /// Unload every Ollama model currently in `/api/ps`. No-op when Ollama is not registered.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::Provider`] when `/api/ps` or an unload request fails.
    pub async fn unload_ollama_models(&self) -> Result<(), LlmError> {
        let Some(client) = self.clients.get("ollama") else {
            return Ok(());
        };
        client.unload_resident_models().await
    }

    /// Completes a chat using the task chain; returns response + winning trace.
    ///
    /// # Errors
    ///
    /// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
    pub async fn complete(
        &self,
        task: TaskKind,
        messages: &[LlmMessage],
    ) -> Result<(LlmResponse, CompletionTrace), LlmError> {
        self.complete_with_tools(task, messages, None, DEFAULT_MAX_TOOL_ROUNDS)
            .await
    }

    /// Completes with an optional tool provider (agent loop on the winning candidate).
    ///
    /// # Errors
    ///
    /// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
    pub async fn complete_with_tools(
        &self,
        task: TaskKind,
        messages: &[LlmMessage],
        tools: Option<&dyn ToolProvider>,
        max_rounds: u32,
    ) -> Result<(LlmResponse, CompletionTrace), LlmError> {
        if self.clients.is_empty() {
            return Err(LlmError::NoProviders);
        }
        let chain = self.chains.chain_for(task);
        if chain.is_empty() {
            return Err(LlmError::Provider(format!(
                "no failover chain configured for task {}",
                task.as_str()
            )));
        }

        let mut failures: Vec<String> = Vec::new();
        let mut attempted = 0u32;
        info!(
            task = task.as_str(),
            route = %format_route(chain),
            tools = tools.is_some(),
            "llm: starting completion"
        );
        for candidate in chain {
            let Some(client) = self.clients.get(&candidate.provider) else {
                debug!(
                    provider = %candidate.provider,
                    model = %candidate.model,
                    task = task.as_str(),
                    "llm: skip route slot (client not registered)"
                );
                failures.push(format!(
                    "{}: skipped (client not registered)",
                    candidate.provider
                ));
                continue;
            };
            attempted += 1;
            if attempted > 1 {
                warn!(
                    provider = %candidate.provider,
                    model = %candidate.model,
                    task = task.as_str(),
                    attempt = attempted,
                    "llm: failover to next route slot"
                );
            } else {
                info!(
                    provider = %candidate.provider,
                    model = %candidate.model,
                    task = task.as_str(),
                    "llm: using route head"
                );
            }
            match chat_with_tools(
                client.as_ref(),
                &candidate.model,
                messages,
                tools,
                max_rounds,
                task.as_str(),
            )
            .await
            {
                Ok((response, trace)) => {
                    info!(
                        provider = %trace.provider,
                        model = %trace.model,
                        task = task.as_str(),
                        prompt_tokens = trace.prompt_tokens,
                        completion_tokens = trace.completion_tokens,
                        "llm: completion ok"
                    );
                    return Ok((response, trace));
                }
                Err(err) => {
                    warn!(
                        provider = %candidate.provider,
                        model = %candidate.model,
                        task = task.as_str(),
                        error = %err,
                        "llm: route slot failed"
                    );
                    failures.push(format!("{}: {err}", candidate.provider));
                }
            }
        }
        Err(LlmError::AllFailed(failures.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::client::LlmUsage;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockClient {
        id: &'static str,
        fail_retryable: bool,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl LlmClient for MockClient {
        fn provider_id(&self) -> &str {
            self.id
        }

        async fn chat(
            &self,
            _messages: &[LlmMessage],
            model: &str,
            _tools: Option<&[crate::llm::client::LlmToolDef]>,
        ) -> Result<LlmResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_retryable {
                return Err(LlmError::Provider("429 rate limit".into()));
            }
            Ok(LlmResponse {
                message: LlmMessage::assistant(format!("ok from {}:{model}", self.id)),
                finish_reason: "stop".into(),
                usage: Some(LlmUsage {
                    prompt_tokens: 3,
                    completion_tokens: 7,
                }),
            })
        }
    }

    #[tokio::test]
    async fn failover_to_second_provider() {
        let a_calls = Arc::new(AtomicUsize::new(0));
        let b_calls = Arc::new(AtomicUsize::new(0));
        let mut clients: HashMap<String, Arc<dyn LlmClient>> = HashMap::new();
        clients.insert(
            "a".into(),
            Arc::new(MockClient {
                id: "a",
                fail_retryable: true,
                calls: a_calls.clone(),
            }),
        );
        clients.insert(
            "b".into(),
            Arc::new(MockClient {
                id: "b",
                fail_retryable: false,
                calls: b_calls.clone(),
            }),
        );
        let chains = TaskChains::new().with_chain(
            TaskKind::Freeform,
            vec![
                ChainCandidate::new("a", "model-a"),
                ChainCandidate::new("b", "model-b"),
            ],
        );
        let router = FailoverRouter::new(clients, chains);
        let (response, trace) = router
            .complete(TaskKind::Freeform, &[LlmMessage::user("hi")])
            .await
            .expect("failover");
        assert_eq!(response.message.content, "ok from b:model-b");
        assert_eq!(trace.provider, "b");
        assert_eq!(trace.model, "model-b");
        assert_eq!(trace.prompt_tokens, 3);
        assert_eq!(trace.completion_tokens, 7);
        assert_eq!(a_calls.load(Ordering::SeqCst), 1);
        assert_eq!(b_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn skips_missing_client() {
        let mut clients: HashMap<String, Arc<dyn LlmClient>> = HashMap::new();
        clients.insert(
            "b".into(),
            Arc::new(MockClient {
                id: "b",
                fail_retryable: false,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        let chains = TaskChains::new().with_chain(
            TaskKind::Freeform,
            vec![
                ChainCandidate::new("a", "model-a"),
                ChainCandidate::new("b", "model-b"),
            ],
        );
        let router = FailoverRouter::new(clients, chains);
        let (_, trace) = router
            .complete(TaskKind::Freeform, &[LlmMessage::user("hi")])
            .await
            .expect("skip missing");
        assert_eq!(trace.provider, "b");
    }

    #[test]
    fn parse_candidate_with_colons_in_model() {
        let c = ChainCandidate::parse("openrouter:meta-llama/llama-3.3-70b-instruct:free")
            .expect("parse");
        assert_eq!(c.provider, "openrouter");
        assert_eq!(c.model, "meta-llama/llama-3.3-70b-instruct:free");
    }

    #[test]
    fn route_uses_comma_separators() {
        let chain = vec![
            ChainCandidate::new("ollama", "gemma4:12b"),
            ChainCandidate::new("openrouter", "x"),
        ];
        assert_eq!(format_route(&chain), "ollama:gemma4:12b, openrouter:x");
    }

    #[test]
    fn chef_skips_unregistered_head() {
        let mut clients: HashMap<String, Arc<dyn LlmClient>> = HashMap::new();
        clients.insert(
            "b".into(),
            Arc::new(MockClient {
                id: "b",
                fail_retryable: false,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        let chains = TaskChains::new().with_chain(
            TaskKind::Freeform,
            vec![
                ChainCandidate::new("a", "model-a"),
                ChainCandidate::new("b", "model-b"),
            ],
        );
        let router = FailoverRouter::new(clients, chains);
        assert_eq!(router.chef_label(TaskKind::Freeform), "b:model-b");
    }

    #[test]
    fn ollama_chat_models_dedupes_fast_dev_style_single_model() {
        let same = "qwen3:8b";
        let chains = TaskChains::new()
            .with_chain(
                TaskKind::Freeform,
                vec![ChainCandidate::new("ollama", same)],
            )
            .with_chain(TaskKind::Load, vec![ChainCandidate::new("ollama", same)])
            .with_chain(TaskKind::Draft, vec![ChainCandidate::new("ollama", same)]);
        let router = FailoverRouter::new(HashMap::new(), chains);
        assert_eq!(router.ollama_chat_models(), vec![same.to_string()]);
    }

    #[test]
    fn ollama_chat_models_keeps_distinct_route_models() {
        let chains = TaskChains::new()
            .with_chain(
                TaskKind::Freeform,
                vec![ChainCandidate::new("ollama", "llama3.1:8b")],
            )
            .with_chain(
                TaskKind::Load,
                vec![ChainCandidate::new("ollama", "qwen3.5:9b")],
            )
            .with_chain(
                TaskKind::Draft,
                vec![ChainCandidate::new("ollama", "gemma4:12b")],
            );
        let router = FailoverRouter::new(HashMap::new(), chains);
        let models = router.ollama_chat_models();
        assert_eq!(models.len(), 3);
        assert!(models.contains(&"llama3.1:8b".into()));
        assert!(models.contains(&"qwen3.5:9b".into()));
        assert!(models.contains(&"gemma4:12b".into()));
    }
}

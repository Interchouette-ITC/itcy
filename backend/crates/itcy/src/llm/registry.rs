// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Builds a `FailoverRouter` from config + provider/model route env.

use crate::config::LlmConfig;
use crate::llm::catalog::{
    get_provider_spec, github_models_allowed_today, live_provider_ids, ProviderKind,
};
use crate::llm::client::LlmClient;
use crate::llm::gemini::GeminiClient;
use crate::llm::ollama::OllamaClient;
use crate::llm::openai_compat::OpenAiCompatibleClient;
use crate::llm::router::{format_route, ChainCandidate, FailoverRouter, TaskChains, TaskKind};
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::Arc;
use tracing::{info, warn};

fn env_nonempty(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn first_env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|n| env_nonempty(n))
}

/// When `FAST_DEV=true` **and** `FAST_DEV_MODEL` is set, force every task route to that
/// one Ollama model (smoke / debug). No default model: without `FAST_DEV_MODEL`, routes
/// stay as configured (same as `FAST_DEV` off). Do not use gemma3:4b: no tools in Ollama.
fn fast_dev_enabled() -> bool {
    env_nonempty("FAST_DEV")
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

fn fast_dev_model() -> Option<String> {
    env_nonempty("FAST_DEV_MODEL")
}

fn apply_fast_dev(route: Vec<ChainCandidate>) -> Vec<ChainCandidate> {
    match (fast_dev_enabled(), fast_dev_model()) {
        (true, Some(model)) => vec![ChainCandidate::new("ollama", model)],
        _ => route,
    }
}

/// Splits on `,` or `>` (both accepted; commas preferred in docs).
fn split_route_entries(raw: &str) -> Vec<String> {
    raw.split([',', '>'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_explicit_route(specs: &[String]) -> Vec<ChainCandidate> {
    specs
        .iter()
        .filter_map(|s| {
            let parsed = ChainCandidate::parse(s);
            if parsed.is_none() {
                warn!(entry = %s, "llm: ignoring invalid route entry (want provider:model)");
            }
            parsed
        })
        .collect()
}

/// Expands `ollama,openrouter,groq` using catalog default models.
fn expand_provider_ranking(raw: &str) -> Vec<ChainCandidate> {
    split_route_entries(raw)
        .into_iter()
        .filter_map(|id| {
            let Some(spec) = get_provider_spec(&id) else {
                warn!(provider = %id, "llm: unknown provider in ranking");
                return None;
            };
            Some(ChainCandidate::new(spec.id, spec.default_model))
        })
        .collect()
}

fn filter_github_from_route(chain: Vec<ChainCandidate>, allow_github: bool) -> Vec<ChainCandidate> {
    if allow_github {
        return chain;
    }
    chain
        .into_iter()
        .filter(|c| {
            if c.provider == "github" {
                warn!(
                    provider = %c.provider,
                    model = %c.model,
                    "llm: dropping github candidate after cutover"
                );
                false
            } else {
                true
            }
        })
        .collect()
}

/// Resolve: explicit `provider:model` route env → provider ranking env → config.toml.
fn resolve_task_route(
    route_envs: &[&str],
    provider_envs: &[&str],
    config_route: &[String],
) -> Vec<ChainCandidate> {
    if let Some(raw) = first_env(route_envs) {
        return parse_explicit_route(&split_route_entries(&raw));
    }
    if let Some(raw) = first_env(provider_envs) {
        return expand_provider_ranking(&raw);
    }
    parse_explicit_route(config_route)
}

fn register_client(
    id: &str,
    clients: &mut HashMap<String, Arc<dyn LlmClient>>,
    skipped: &mut Vec<String>,
) {
    let Some(spec) = get_provider_spec(id) else {
        skipped.push(format!("{id} (unknown provider)"));
        return;
    };
    match spec.kind {
        ProviderKind::Ollama => {
            let base = env_nonempty("OLLAMA_BASE_URL")
                .unwrap_or_else(|| spec.default_base_url.to_string());
            clients.insert(spec.id.to_string(), Arc::new(OllamaClient::new(base)));
        }
        ProviderKind::OpenAiCompatible => {
            let Some(key) = env_nonempty(spec.env_api_key) else {
                skipped.push(format!("{} (unset {})", spec.id, spec.env_api_key));
                return;
            };
            clients.insert(
                spec.id.to_string(),
                Arc::new(OpenAiCompatibleClient::new(
                    spec.id,
                    key,
                    spec.default_base_url,
                )),
            );
        }
        ProviderKind::Gemini => {
            let Some(key) = env_nonempty(spec.env_api_key) else {
                skipped.push(format!("{} (unset {})", spec.id, spec.env_api_key));
                return;
            };
            clients.insert(
                spec.id.to_string(),
                Arc::new(GeminiClient::new(key, spec.default_base_url)),
            );
        }
    }
}

/// Resolves freeform / load / draft routes from env + config (with `FastDev` + GitHub filter).
fn resolve_all_task_routes(
    config: &LlmConfig,
    allow_github: bool,
) -> (
    Vec<ChainCandidate>,
    Vec<ChainCandidate>,
    Vec<ChainCandidate>,
) {
    let freeform_route = apply_fast_dev(filter_github_from_route(
        resolve_task_route(
            &["LLM_FREEFORM_ROUTE", "LLM_CHAT_CHAIN"],
            &["LLM_FREEFORM_PROVIDERS", "LLM_CHAT_PREFERENCE"],
            &config.freeform_route,
        ),
        allow_github,
    ));
    let load_route = apply_fast_dev(filter_github_from_route(
        resolve_task_route(
            &["LLM_LOAD_ROUTE"],
            &["LLM_LOAD_PROVIDERS"],
            &config.load_route,
        ),
        allow_github,
    ));
    let draft_route = apply_fast_dev(filter_github_from_route(
        resolve_task_route(
            &["LLM_DRAFT_ROUTE", "LLM_DRAFT_CHAIN"],
            &["LLM_DRAFT_PROVIDERS", "LLM_DRAFT_PREFERENCE"],
            &config.draft_route,
        ),
        allow_github,
    ));
    (freeform_route, load_route, draft_route)
}

fn provider_has_credentials(id: &str) -> bool {
    match get_provider_spec(id).map(|s| s.kind) {
        Some(ProviderKind::Ollama) => true,
        Some(ProviderKind::OpenAiCompatible | ProviderKind::Gemini) => {
            get_provider_spec(id).is_some_and(|s| env_nonempty(s.env_api_key).is_some())
        }
        None => false,
    }
}

/// Registers clients for providers named in the routes; logs skip / outside-pool notes.
fn register_route_clients(
    allow_github: bool,
    freeform_route: &[ChainCandidate],
    load_route: &[ChainCandidate],
    draft_route: &[ChainCandidate],
) -> HashMap<String, Arc<dyn LlmClient>> {
    let mut needed: HashSet<String> = HashSet::new();
    for c in freeform_route
        .iter()
        .chain(load_route.iter())
        .chain(draft_route.iter())
    {
        needed.insert(c.provider.clone());
    }

    let mut clients: HashMap<String, Arc<dyn LlmClient>> = HashMap::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut outside_routes: Vec<String> = Vec::new();

    for id in live_provider_ids(allow_github) {
        if !needed.contains(id) {
            if provider_has_credentials(id) {
                outside_routes.push(id.to_string());
            }
            continue;
        }
        register_client(id, &mut clients, &mut skipped);
    }

    if !allow_github {
        info!("llm: GitHub Models cutover active; github not registered");
    }
    if !outside_routes.is_empty() {
        info!(
            providers = %outside_routes.join(", "),
            "llm: credentials present but not in routes (not in provider pool)"
        );
    }
    if !skipped.is_empty() {
        warn!(
            skipped = %skipped.join(", "),
            "llm: route slots missing from provider pool"
        );
    }
    clients
}

fn route_head_label(
    route: &[ChainCandidate],
    clients: &HashMap<String, Arc<dyn LlmClient>>,
) -> String {
    route
        .iter()
        .find(|c| clients.contains_key(&c.provider))
        .map_or_else(|| "(none)".into(), ChainCandidate::label)
}

fn log_route_heads(
    clients: &HashMap<String, Arc<dyn LlmClient>>,
    freeform_route: &[ChainCandidate],
    load_route: &[ChainCandidate],
    draft_route: &[ChainCandidate],
) {
    let providers: Vec<&str> = {
        let mut ids: Vec<&str> = clients.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    };
    info!(providers = %providers.join(", "), "llm: provider pool");
    info!(
        head = %route_head_label(freeform_route, clients),
        route = %format_route(freeform_route),
        "llm: freeform route (Slack replies)"
    );
    info!(
        head = %route_head_label(load_route, clients),
        route = %format_route(load_route),
        "llm: load route (research pack / candidates)"
    );
    info!(
        head = %route_head_label(draft_route, clients),
        route = %format_route(draft_route),
        "llm: draft route (LinkedIn writer)"
    );
}

/// Registers only providers named in the freeform/load/draft routes (pool = route providers).
#[must_use]
pub fn build_router(config: &LlmConfig) -> FailoverRouter {
    let allow_github = github_models_allowed_today();
    let (freeform_route, load_route, draft_route) = resolve_all_task_routes(config, allow_github);

    match (fast_dev_enabled(), fast_dev_model()) {
        (true, Some(model)) => {
            info!(
                model = %model,
                "llm: FAST_DEV=true - all routes forced to ollama:<FAST_DEV_MODEL>"
            );
        }
        (true, None) => {
            warn!("llm: FAST_DEV=true but FAST_DEV_MODEL unset; leaving configured routes");
        }
        _ => {}
    }

    let clients = register_route_clients(allow_github, &freeform_route, &load_route, &draft_route);
    log_route_heads(&clients, &freeform_route, &load_route, &draft_route);

    if clients.is_empty() {
        warn!("llm: provider pool empty; model-backed replies will error");
    }

    let chains = TaskChains::new()
        .with_chain(TaskKind::Freeform, freeform_route)
        .with_chain(TaskKind::Load, load_route)
        .with_chain(TaskKind::Draft, draft_route);

    FailoverRouter::new(clients, chains)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_dev_forces_configured_model() {
        std::env::set_var("FAST_DEV", "true");
        std::env::remove_var("FAST_DEV_MODEL");
        let route = apply_fast_dev(vec![
            ChainCandidate::new("ollama", "qwen3.5:9b"),
            ChainCandidate::new("ollama", "llama3.1:8b"),
        ]);
        assert_eq!(route.len(), 2);
        assert_eq!(route[0].model, "qwen3.5:9b");
        std::env::set_var("FAST_DEV_MODEL", "qwen3.5:4b");
        let route2 = apply_fast_dev(vec![ChainCandidate::new("ollama", "qwen3.5:9b")]);
        std::env::remove_var("FAST_DEV");
        std::env::remove_var("FAST_DEV_MODEL");
        assert_eq!(route2[0].model, "qwen3.5:4b");
    }

    #[test]
    fn parse_default_style_route() {
        let specs = vec![
            "groq:llama-3.3-70b-versatile".into(),
            "openrouter:meta-llama/llama-3.3-70b-instruct:free".into(),
        ];
        let chain = parse_explicit_route(&specs);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[1].model, "meta-llama/llama-3.3-70b-instruct:free");
    }

    #[test]
    fn split_accepts_comma_and_gt() {
        let entries = split_route_entries("ollama:gemma4:12b, groq:llama-3.3-70b-versatile");
        assert_eq!(entries.len(), 2);
        let entries_gt = split_route_entries("ollama:gemma4:12b > groq:x");
        assert_eq!(entries_gt.len(), 2);
    }

    #[test]
    fn provider_ranking_expands_defaults() {
        let chain = expand_provider_ranking("ollama,openrouter");
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].provider, "ollama");
        assert_eq!(chain[0].model, "gemma4:12b");
        assert_eq!(chain[1].provider, "openrouter");
    }

    #[test]
    fn filter_removes_github() {
        let chain = vec![
            ChainCandidate::new("groq", "m"),
            ChainCandidate::new("github", "x"),
        ];
        let filtered = filter_github_from_route(chain, false);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].provider, "groq");
    }
}

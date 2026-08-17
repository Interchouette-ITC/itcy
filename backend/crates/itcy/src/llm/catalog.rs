// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Provider catalog and GitHub Models cutover gate.

use chrono::{Local, NaiveDate};

/// Last calendar day GitHub Models may appear in the live path (inclusive).
pub const GITHUB_MODELS_CUTOVER_DATE: NaiveDate = match NaiveDate::from_ymd_opt(2026, 7, 31) {
    Some(d) => d,
    None => unreachable!(),
};

/// How a provider is talked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAiCompatible,
    Ollama,
    Gemini,
}

/// Static provider metadata.
#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    pub id: &'static str,
    pub kind: ProviderKind,
    pub default_base_url: &'static str,
    /// Empty for ollama (no API key).
    pub env_api_key: &'static str,
    /// Used when expanding `LLM_*_PREFERENCE=ollama>groq>...` (no model in the entry).
    pub default_model: &'static str,
}

const PROVIDER_CATALOG: &[ProviderSpec] = &[
    ProviderSpec {
        id: "groq",
        kind: ProviderKind::OpenAiCompatible,
        default_base_url: "https://api.groq.com/openai/v1",
        env_api_key: "GROQ_API_KEY",
        default_model: "llama-3.3-70b-versatile",
    },
    ProviderSpec {
        id: "openrouter",
        kind: ProviderKind::OpenAiCompatible,
        default_base_url: "https://openrouter.ai/api/v1",
        env_api_key: "OPENROUTER_API_KEY",
        default_model: "meta-llama/llama-3.3-70b-instruct:free",
    },
    ProviderSpec {
        id: "github",
        kind: ProviderKind::OpenAiCompatible,
        default_base_url: "https://models.github.ai/inference",
        env_api_key: "GITHUB_MODELS_API_KEY",
        default_model: "openai/gpt-4o-mini",
    },
    ProviderSpec {
        id: "gemini",
        kind: ProviderKind::Gemini,
        default_base_url: "https://generativelanguage.googleapis.com/v1beta",
        env_api_key: "GEMINI_API_KEY",
        default_model: "gemini-2.5-flash",
    },
    ProviderSpec {
        id: "cerebras",
        kind: ProviderKind::OpenAiCompatible,
        default_base_url: "https://api.cerebras.ai/v1",
        env_api_key: "CEREBRAS_API_KEY",
        default_model: "gpt-oss-120b",
    },
    ProviderSpec {
        id: "ollama",
        kind: ProviderKind::Ollama,
        default_base_url: "http://127.0.0.1:11434",
        env_api_key: "",
        default_model: "gemma4:12b",
    },
];

/// Looks up a provider by id.
#[must_use]
pub fn get_provider_spec(id: &str) -> Option<&'static ProviderSpec> {
    PROVIDER_CATALOG.iter().find(|p| p.id == id)
}

/// True when GitHub Models may still be registered for the given local calendar date.
#[must_use]
pub fn github_models_allowed_on(date: NaiveDate) -> bool {
    date <= GITHUB_MODELS_CUTOVER_DATE
}

/// True when GitHub Models may still be registered today (host local date).
#[must_use]
pub fn github_models_allowed_today() -> bool {
    github_models_allowed_on(Local::now().date_naive())
}

/// Live catalog ids for registration (excludes `github` after cutover).
#[must_use]
pub fn live_provider_ids(allow_github: bool) -> Vec<&'static str> {
    PROVIDER_CATALOG
        .iter()
        .filter(|p| p.id != "github" || allow_github)
        .map(|p| p.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_allowed_until_cutover_inclusive() {
        assert!(github_models_allowed_on(
            NaiveDate::from_ymd_opt(2026, 7, 31).unwrap()
        ));
        assert!(!github_models_allowed_on(
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        ));
    }

    #[test]
    fn live_catalog_drops_github_after_cutover() {
        let with_gh = live_provider_ids(true);
        assert!(with_gh.contains(&"github"));
        let without = live_provider_ids(false);
        assert!(!without.contains(&"github"));
        assert!(without.contains(&"groq"));
        assert!(without.contains(&"ollama"));
    }
}

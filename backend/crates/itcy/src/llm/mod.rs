// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Model router, failover, and provider clients.

pub mod agent;
pub mod catalog;
pub mod client;
pub mod clock;
pub mod disclosure;
pub mod gemini;
pub mod ollama;
pub mod openai_compat;
pub mod prompt_dump;
pub mod registry;
pub mod router;
pub mod sanitize;

pub use agent::{chat_with_tools, ToolProvider, DEFAULT_MAX_TOOL_ROUNDS};
pub use client::{
    CompletionTrace, LlmClient, LlmError, LlmMessage, LlmResponse, LlmToolCall, LlmToolDef,
    LlmUsage,
};
pub use clock::{today_context_line, today_prompt_date};
pub use disclosure::{
    ensure_stored_disclosure, format_disclosure, strip_trailing_disclosures, with_disclosure,
};
pub use registry::build_router;
pub use router::{ChainCandidate, FailoverRouter, TaskChains, TaskKind};
pub use sanitize::{
    count_emoji, expand_emoji_shortcodes, instructions_ask_for_emoji, sanitize_itcy_text,
    text_contains_emoji, tweet_emoji_ok,
};

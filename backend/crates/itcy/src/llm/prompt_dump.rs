// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Dump the real message list sent to models (`llm-logs` under the product root).

use crate::llm::client::LlmMessage;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tracing::warn;

static SESSION_PROMPT_DIR: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

fn session_slot() -> &'static Mutex<Option<PathBuf>> {
    SESSION_PROMPT_DIR.get_or_init(|| Mutex::new(None))
}

/// While a research session is live, also write `{task}_llm_prompt.txt` into that folder.
pub fn set_session_prompt_dir(dir: Option<PathBuf>) {
    if let Ok(mut g) = session_slot().lock() {
        *g = dir;
    }
}

/// Latest-overwrite dir for load/draft/freeform prompt dumps (`pw/llm-logs/`).
#[must_use]
pub fn resolve_llm_logs_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ITCY_LLM_LOGS_DIR") {
        let t = p.trim();
        if !t.is_empty() {
            return PathBuf::from(t);
        }
    }
    if Path::new("pw").is_dir() {
        return PathBuf::from("pw/llm-logs");
    }
    if Path::new("../pw").is_dir() {
        return PathBuf::from("../pw/llm-logs");
    }
    crate::paths::product_join("pw/llm-logs")
}

fn format_messages(provider: &str, model: &str, messages: &[LlmMessage]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "provider: {provider}\nmodel: {model}\nts: {}",
        chrono::Local::now().to_rfc3339()
    );
    let _ = writeln!(out);
    for m in messages {
        let _ = writeln!(out, "=== {} ===", m.role.as_str().to_ascii_uppercase());
        if let Some(id) = &m.tool_call_id {
            let _ = writeln!(out, "tool_call_id: {id}");
        }
        if let Some(calls) = &m.tool_calls {
            for tc in calls {
                let _ = writeln!(
                    out,
                    "tool_call: name={} id={} arguments={}",
                    tc.name, tc.id, tc.arguments
                );
            }
        }
        out.push_str(&m.content);
        if !m.content.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// Overwrite `pw/llm-logs/{task}_llm_prompt.txt` (and session copy when set).
pub fn dump_llm_prompt(task: &str, provider: &str, model: &str, messages: &[LlmMessage]) {
    let body = format_messages(provider, model, messages);
    let file_name = format!("{task}_llm_prompt.txt");
    let dir = resolve_llm_logs_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!(error = %e, dir = %dir.display(), "llm: failed to create llm-logs dir");
        return;
    }
    let path = dir.join(&file_name);
    if let Err(e) = std::fs::write(&path, &body) {
        warn!(error = %e, path = %path.display(), "llm: failed to write prompt dump");
    }
    let session = session_slot().lock().ok().and_then(|g| g.clone());
    if let Some(session_dir) = session {
        let path = session_dir.join(&file_name);
        if let Err(e) = std::fs::write(&path, &body) {
            warn!(
                error = %e,
                path = %path.display(),
                "llm: failed to write session prompt dump"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::client::LlmMessage;

    #[test]
    fn format_includes_roles() {
        let text = format_messages(
            "ollama",
            "qwen",
            &[LlmMessage::system("sys"), LlmMessage::user("hi")],
        );
        assert!(text.contains("=== SYSTEM ==="));
        assert!(text.contains("=== USER ==="));
        assert!(text.contains("provider: ollama"));
        assert!(text.contains("model: qwen"));
    }
}

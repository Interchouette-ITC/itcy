// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Contract disclosure line for model-backed replies.

use crate::llm::client::CompletionTrace;
use crate::llm::sanitize::sanitize_itcy_text;

/// Prefix of the contract disclosure line (models sometimes echo it from history).
pub const DISCLOSURE_PREFIX: &str = "Written by AI - ITCy - model ";

/// Formats the mandatory AI disclosure footer.
#[must_use]
pub fn format_disclosure(trace: &CompletionTrace) -> String {
    format_disclosure_parts(
        &trace.model_label(),
        trace.prompt_tokens,
        trace.completion_tokens,
    )
}

/// Disclosure line from stored draft/tweet columns (no live `CompletionTrace`).
#[must_use]
pub fn format_disclosure_parts(model_label: &str, tokens_in: u32, tokens_out: u32) -> String {
    let label = disclosure_model_label(model_label);
    format!("{DISCLOSURE_PREFIX}{label} - tokens in:{tokens_in} out:{tokens_out}")
}

/// Disclosure for manual `LinkedIn` paste: token counts in angle brackets after the model.
#[must_use]
pub fn format_disclosure_paste(model_label: &str, tokens_in: u32, tokens_out: u32) -> String {
    let label = disclosure_model_label(model_label);
    format!("{DISCLOSURE_PREFIX}{label} <in:{tokens_in} out:{tokens_out}>")
}

/// Prefer the writer/rework segment from a compound `model` column (`load=… | tweet=…`).
#[must_use]
pub fn disclosure_model_label(model: &str) -> &str {
    let m = model.trim();
    for key in ["tweet=", "draft=", "rework=", "farce="] {
        if let Some(i) = m.rfind(key) {
            let rest = m[i + key.len()..].trim();
            if !rest.is_empty() {
                return rest;
            }
        }
    }
    if m.is_empty() {
        "unknown"
    } else {
        m
    }
}

/// Re-attach disclosure from stored `model` / token columns after `/change_url`, `/show`, etc.
///
/// Compose helpers rebuild the Link footer and drop the trailing `Written by AI` line; this
/// restores tokens from SQLite so operator commands do not wipe the count.
#[must_use]
pub fn ensure_stored_disclosure(
    body: &str,
    model: &str,
    tokens_in: u32,
    tokens_out: u32,
) -> String {
    let clean = strip_trailing_disclosures(body).trim_end();
    if model.trim().is_empty() && tokens_in == 0 && tokens_out == 0 {
        return clean.to_string();
    }
    format!(
        "{clean}\n\n{}",
        format_disclosure_parts(model, tokens_in, tokens_out)
    )
}

/// Drops trailing paragraphs that look like our disclosure (model echo / history).
#[must_use]
pub fn strip_trailing_disclosures(body: &str) -> &str {
    let mut end = body.trim_end();
    while let Some(idx) = end.rfind(DISCLOSURE_PREFIX) {
        let after_prefix = &end[idx..];
        let line_end = after_prefix.find('\n').unwrap_or(after_prefix.len());
        // Only strip when the match is a trailing paragraph (only whitespace after).
        if after_prefix[line_end..].trim().is_empty() {
            end = end[..idx].trim_end();
            continue;
        }
        break;
    }
    end
}

/// Sanitizes body (no em dash) and appends disclosure as a trailing paragraph.
#[must_use]
pub fn with_disclosure(body: &str, trace: &CompletionTrace) -> String {
    let clean = sanitize_itcy_text(strip_trailing_disclosures(body));
    format!("{}\n\n{}", clean, format_disclosure(trace))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::client::CompletionTrace;
    use crate::llm::sanitize::EM_DASH;

    #[test]
    fn disclosure_matches_contract_shape() {
        let trace = CompletionTrace {
            provider: "groq".into(),
            model: "llama-3.3-70b-versatile".into(),
            prompt_tokens: 12,
            completion_tokens: 34,
        };
        let line = format_disclosure(&trace);
        assert_eq!(
            line,
            "Written by AI - ITCy - model groq/llama-3.3-70b-versatile - tokens in:12 out:34"
        );
        assert_eq!(
            format_disclosure_paste("groq/llama-3.3-70b-versatile", 12, 34),
            "Written by AI - ITCy - model groq/llama-3.3-70b-versatile <in:12 out:34>"
        );
        let full = with_disclosure("Hello", &trace);
        assert!(full.starts_with("Hello\n\nWritten by AI"));
    }

    #[test]
    fn with_disclosure_strips_em_dash() {
        let trace = CompletionTrace {
            provider: "ollama".into(),
            model: "qwen3:8b".into(),
            prompt_tokens: 1,
            completion_tokens: 1,
        };
        let full = with_disclosure(&format!("leap{EM_DASH}growth"), &trace);
        assert!(!full.contains(EM_DASH));
        assert!(full.contains("leap, growth"));
        assert!(!full.contains("leap - growth"));
    }

    #[test]
    fn with_disclosure_strips_model_echoed_footers() {
        let trace = CompletionTrace {
            provider: "ollama".into(),
            model: "qwen3:8b".into(),
            prompt_tokens: 4033,
            completion_tokens: 133,
        };
        let body = "Ready for review.\n\n\
Written by AI - ITCy - model ollama/qwen3:8b - tokens in:3858 out:102\n\n\
Written by AI - ITCy - model ollama/qwen3:8b - tokens in:3900 out:110";
        let full = with_disclosure(body, &trace);
        assert_eq!(
            full.matches(DISCLOSURE_PREFIX).count(),
            1,
            "only the real footer should remain: {full}"
        );
        assert!(
            full.ends_with("Written by AI - ITCy - model ollama/qwen3:8b - tokens in:4033 out:133")
        );
        assert!(full.starts_with("Ready for review."));
    }

    #[test]
    fn with_disclosure_expands_emoji_shortcodes() {
        let trace = CompletionTrace {
            provider: "ollama".into(),
            model: "llama3.1:8b".into(),
            prompt_tokens: 1,
            completion_tokens: 1,
        };
        let full = with_disclosure("Ship it :owl::rocket:", &trace);
        assert!(full.contains('🦉'));
        assert!(full.contains('🚀'));
        assert!(!full.contains(":owl:"));
        assert!(!full.contains(":rocket:"));
    }

    #[test]
    fn ensure_stored_disclosure_restores_tokens_after_compose() {
        let composed = "Tweet ID: TWEET-1\n\nHello.\n\nLink: 1\n0 = no link. /change_url TWEET-1 <0|1|2|3|url>\n1. https://example.com/a";
        let out = ensure_stored_disclosure(
            composed,
            "load=ollama/qwen3:8b | tweet=ollama/qwen3:8b",
            6146,
            119,
        );
        assert!(
            out.contains("tokens in:6146 out:119"),
            "tokens must come back from columns: {out}"
        );
        assert!(out.contains("model ollama/qwen3:8b"));
        assert_eq!(out.matches(DISCLOSURE_PREFIX).count(), 1);
    }
}

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Farce tweet writer: joke BAT without SERP LOAD (`/tweet_farce`).

use crate::llm::client::{CompletionTrace, LlmMessage};
use crate::llm::clock::today_context_line;
use crate::llm::disclosure::with_disclosure;
use crate::llm::router::{FailoverRouter, TaskKind};
use crate::prompts::{
    tweet_farce_user_message, AI_CMO, CREATIVE_X, FORM_CRAFT_X, TWEET_FARCE_SYSTEM_CORE, WHO_IS_WHO,
};
use crate::sources::rag::{
    begin_load_session_dir, end_session_best_effort, resolve_session_draft_id, GroundedDraft,
    RagError,
};
use crate::sources::tweet_footer::{
    aerate_tweet_commentary, compose_tweet_message, strip_own_x_handle, tweet_body_exploded,
};
use crate::tools::ItcyTools;
use std::path::{Path, PathBuf};
use tracing::info;

const FARCE_HANDLES: [&str; 3] = ["@grok", "@cursor_ai", "@elonmusk"];

fn farce_system_prompt() -> String {
    format!(
        "{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}",
        today_context_line(),
        WHO_IS_WHO,
        AI_CMO,
        CREATIVE_X,
        FORM_CRAFT_X,
        TWEET_FARCE_SYSTEM_CORE
    )
}

/// True when `text` mentions every locked farce handle as a whole @token.
#[must_use]
pub fn farce_has_required_mentions(text: &str) -> bool {
    FARCE_HANDLES.iter().all(|h| has_handle_token(text, h))
}

/// Append any missing locked handles so ship never depends on the model alone.
#[must_use]
pub fn ensure_farce_mentions(body: &str) -> String {
    let body = body.trim();
    let missing: Vec<&str> = FARCE_HANDLES
        .iter()
        .copied()
        .filter(|h| !has_handle_token(body, h))
        .collect();
    if missing.is_empty() {
        return body.to_string();
    }
    let tag = missing.join(" ");
    if body.is_empty() {
        // Still emit the full trio when the writer returned nothing usable.
        FARCE_HANDLES.join(" ")
    } else {
        format!("{body}\n\n{tag}")
    }
}

fn has_handle_token(text: &str, handle: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let needle = handle.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let n = needle.as_bytes();
    let mut start = 0;
    while start + n.len() <= bytes.len() {
        if let Some(rel) = lower[start..].find(&needle) {
            let idx = start + rel;
            let after = bytes.get(idx + n.len()).copied();
            let ok_after = after.is_none_or(|b| !b.is_ascii_alphanumeric() && b != b'_');
            if ok_after {
                return true;
            }
            start = idx + 1;
        } else {
            break;
        }
    }
    false
}

fn farce_research_pack(theme: &str) -> String {
    let theme = theme.trim();
    let subject = if theme.is_empty() {
        "(operator left theme open)"
    } else {
        theme
    };
    format!(
        "## ResearchPack\n\
mode: tweet_farce\n\
subject: {subject}\n\
summary: joke tweet; no SERP; tag @grok @cursor_ai @elonmusk\n\
candidates:\n\
rejected:\n\
notes: Link 0; no cite URL\n"
    )
}

fn scrub_farce_body(raw: &str) -> String {
    let raw = crate::llm::sanitize_itcy_text(raw);
    let mut lines: Vec<&str> = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if t.starts_with("http://") || t.starts_with("https://") {
            continue;
        }
        if t.eq_ignore_ascii_case("sources:") || t.to_ascii_lowercase().starts_with("sources:") {
            break;
        }
        lines.push(line);
    }
    strip_own_x_handle(&aerate_tweet_commentary(&lines.join("\n")))
}

/// Build an open `TWEET-` farce (no LOAD / SERP). Theme hint may be empty.
///
/// # Errors
///
/// Returns [`RagError`] when the LLM fails or the body is not a tweet.
pub async fn build_farce_tweet(
    router: &FailoverRouter,
    db_path: &Path,
    theme: &str,
    tools: Option<&ItcyTools>,
) -> Result<GroundedDraft, RagError> {
    let theme = theme.trim();
    let subject = if theme.is_empty() {
        "farce".to_string()
    } else {
        theme.to_string()
    };
    let research_pack = farce_research_pack(theme);
    let session_dir = begin_load_session_dir(tools, db_path, &subject).await;
    let (body_raw, tweet_trace) =
        run_farce_phase(router, theme, session_dir.as_ref(), tools).await?;
    let body = ensure_farce_mentions(&scrub_farce_body(&body_raw));
    if tweet_body_exploded(&body) {
        end_session_best_effort(tools, session_dir.as_ref(), "farce: exploded essay").await;
        return Err(RagError::NotATweet);
    }
    debug_assert!(farce_has_required_mentions(&body));
    let tweet_id = resolve_session_draft_id(tools, db_path).await;
    end_session_best_effort(
        tools,
        session_dir.as_ref(),
        &session_dir.as_ref().map_or_else(
            || "session_dir: (none)".into(),
            |d| format!("session_dir: {}", d.display()),
        ),
    )
    .await;
    let link_options: Vec<String> = Vec::new();
    let composed = compose_tweet_message(&body, &tweet_id, &link_options);
    info!(tweet_id = %tweet_id, "farce: tweet open");
    Ok(GroundedDraft {
        subject,
        body: with_disclosure(&composed, &tweet_trace),
        draft_id: tweet_id,
        model: format!("farce={}", tweet_trace.model_label()),
        tokens_in: tweet_trace.prompt_tokens,
        tokens_out: tweet_trace.completion_tokens,
        source_labels: vec!["tweet_farce (no cite)".into()],
        link_options,
        research_pack,
    })
}

async fn run_farce_phase(
    router: &FailoverRouter,
    theme: &str,
    session_dir: Option<&PathBuf>,
    tools: Option<&ItcyTools>,
) -> Result<(String, CompletionTrace), RagError> {
    crate::sources::rag::log_pipeline_banner("TWEET FARCE (writer)");
    let user = tweet_farce_user_message(theme);
    let messages = vec![
        LlmMessage::system(farce_system_prompt()),
        LlmMessage::user(user.clone()),
    ];
    let (response, trace) = match router
        .complete_with_tools(TaskKind::Draft, &messages, None, 0)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            end_session_best_effort(tools, session_dir, &format!("farce writer failed: {e}")).await;
            return Err(e.into());
        }
    };
    let body = scrub_farce_body(&response.message.content);
    if !tweet_body_exploded(&body) {
        // Mentions are ensured after this phase; one pass is enough when the gag is short.
        return Ok((body, trace));
    }
    let retry_user = format!(
        "{user}\n\nPREVIOUS OUTPUT REJECTED (essay). Output ONLY a short joke tweet \
(target 280). First line: @grok @cursor_ai @elonmusk. No https. No essay."
    );
    let retry_messages = vec![
        LlmMessage::system(farce_system_prompt()),
        LlmMessage::user(retry_user),
    ];
    let (response, trace) = match router
        .complete_with_tools(TaskKind::Draft, &retry_messages, None, 0)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            end_session_best_effort(tools, session_dir, &format!("farce retry failed: {e}")).await;
            return Err(e.into());
        }
    };
    Ok((scrub_farce_body(&response.message.content), trace))
}

/// True when a stored tweet was built by `/tweet_farce` (pack stamp or handles).
#[must_use]
pub fn stored_is_farce(research_pack: &str, body: &str) -> bool {
    research_pack.contains("mode: tweet_farce") || farce_has_required_mentions(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_handles_must_all_appear() {
        let ok = "hey @grok @cursor_ai and @elonmusk\n\n#DadJoke";
        assert!(farce_has_required_mentions(ok));
        assert!(!farce_has_required_mentions("only @grok and @elonmusk"));
        assert!(!farce_has_required_mentions(
            "@grokking @cursor_ai @elonmusk"
        ));
    }

    #[test]
    fn scrub_drops_https_and_sources() {
        let raw = "hi @grok @cursor_ai @elonmusk\n\nhttps://example.com\n\nSources:\n- x";
        let out = scrub_farce_body(raw);
        assert!(!out.contains("https://"));
        assert!(!out.contains("Sources"));
        assert!(farce_has_required_mentions(&out));
    }

    #[test]
    fn pack_stamp_marks_farce() {
        let pack = farce_research_pack("Mars Wi-Fi");
        assert!(stored_is_farce(&pack, "no handles yet"));
        assert!(stored_is_farce("", "x @grok y @cursor_ai z @elonmusk"));
    }

    #[test]
    fn ensure_appends_only_missing_handles() {
        let partial = "Wi-Fi on Mars still needs a punchline for @grok.";
        let out = ensure_farce_mentions(partial);
        assert!(farce_has_required_mentions(&out));
        assert!(out.contains("@cursor_ai"));
        assert!(out.contains("@elonmusk"));
        assert!(out.starts_with("Wi-Fi on Mars"));
        let full = ensure_farce_mentions("hey @grok @cursor_ai @elonmusk");
        assert_eq!(full, "hey @grok @cursor_ai @elonmusk");
        assert!(farce_has_required_mentions(&ensure_farce_mentions("")));
    }
}

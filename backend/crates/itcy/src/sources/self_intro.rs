// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Self-introduction writers for `/draft_about_itcy` (`LinkedIn`) and `/tweet_about_itcy` (X).
//!
//! Uses a static identity pack (no HTTP) and dedicated self-intro prompts. The BAT store,
//! session log, and `GroundedDraft` shape are the same as the catalog-commentary path.

use crate::llm::agent::ToolProvider;
use crate::llm::client::{CompletionTrace, LlmMessage};
use crate::llm::clock::today_context_line;
use crate::llm::disclosure::with_disclosure;
use crate::llm::router::{FailoverRouter, TaskKind};
use crate::prompts::{
    self_user_message, AI_CMO, CREATIVE_LINKEDIN, CREATIVE_X, FORM_CRAFT_LINKEDIN, FORM_CRAFT_X,
    SELF_SYSTEM_CORE, WHO_IS_WHO,
};
use crate::sources::itc_digest::build_itcy_self_pack;
use crate::sources::rag::{
    begin_load_session_dir, checkpoint_building_pack, end_session_best_effort, log_pipeline_banner,
    resolve_session_draft_id, GroundedDraft, RagError, MAX_TOOL_ROUNDS,
};
use crate::sources::tweet_footer::{
    coerce_tweet_body, ensure_tweet_cite_line, in_tweet_publisher_url, pick_tweet_cite_options,
    strip_own_x_handle, tweet_body_exploded,
};
use crate::tools::ItcyTools;
use std::path::{Path, PathBuf};
use tracing::warn;

fn self_draft_system_prompt() -> String {
    format!(
        "{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}",
        today_context_line(),
        WHO_IS_WHO,
        AI_CMO,
        CREATIVE_LINKEDIN,
        FORM_CRAFT_LINKEDIN,
        SELF_SYSTEM_CORE
    )
}

fn self_tweet_system_prompt() -> String {
    format!(
        "{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}",
        today_context_line(),
        WHO_IS_WHO,
        AI_CMO,
        CREATIVE_X,
        FORM_CRAFT_X,
        SELF_SYSTEM_CORE
    )
}

fn session_note(session_dir: Option<&PathBuf>) -> String {
    session_dir.map_or_else(
        || "session_dir: (none)".into(),
        |d| format!("session_dir: {}", d.display()),
    )
}

/// `LinkedIn` self-introduction draft for `/draft_about_itcy`.
///
/// # Errors
///
/// Returns a [`RagError`] on LLM or store failure.
pub async fn build_itcy_self_draft(
    router: &FailoverRouter,
    db_path: &Path,
    instructions: &str,
    tools: Option<&ItcyTools>,
) -> Result<GroundedDraft, RagError> {
    let subject = "ITCy self-introduction";
    let (research_pack, pack_urls) = build_itcy_self_pack();
    let session_dir = begin_load_session_dir(tools, db_path, subject).await;
    let tools_dyn: Option<&dyn ToolProvider> = tools.map(|t| t as &dyn ToolProvider);
    checkpoint_building_pack(db_path, tools, subject, &research_pack, &pack_urls).await;
    if let Some(t) = tools {
        t.set_draft_policy(&pack_urls).await;
    }
    log_pipeline_banner("SELF-INTRO DRAFT (LinkedIn writer)");
    let messages = vec![
        LlmMessage::system(self_draft_system_prompt()),
        LlmMessage::user(self_user_message("linkedin", instructions)),
    ];
    let (response, trace) = match router
        .complete_with_tools(TaskKind::Draft, &messages, tools_dyn, MAX_TOOL_ROUNDS)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            end_session_best_effort(
                tools,
                session_dir.as_ref(),
                &format!("self draft failed: {e}"),
            )
            .await;
            return Err(e.into());
        }
    };
    let draft_id = resolve_session_draft_id(tools, db_path).await;
    end_session_best_effort(
        tools,
        session_dir.as_ref(),
        &session_note(session_dir.as_ref()),
    )
    .await;
    let body = crate::sources::rag::scrub_and_validate_writer_body(
        &response.message.content,
        &pack_urls,
        &crate::sources::rag::paste_subject_line(subject),
    )?;
    let body = crate::sources::handles::ensure_linkedin_brand_mention(&body);
    let mut link_options = crate::sources::draft_footer::pick_link_options(&pack_urls, &body);
    body_ensure_repo_cite(&mut link_options);
    let body = crate::sources::draft_footer::ensure_primary_link_line(
        &body,
        link_options.first().map(String::as_str),
    );
    let body = crate::sources::draft_footer::compose_self_intro_draft_message(&body, &draft_id);
    Ok(GroundedDraft {
        subject: subject.to_string(),
        body: with_disclosure(&body, &trace),
        draft_id,
        model: format!("self-intro | draft={}", trace.model_label()),
        tokens_in: trace.prompt_tokens,
        tokens_out: trace.completion_tokens,
        source_labels: pack_urls.clone(),
        link_options,
        research_pack,
    })
}

/// X self-introduction tweet for `/tweet_about_itcy`.
///
/// # Errors
///
/// Returns a [`RagError`] on LLM or store failure.
pub async fn build_itcy_self_tweet(
    router: &FailoverRouter,
    db_path: &Path,
    instructions: &str,
    tools: Option<&ItcyTools>,
) -> Result<GroundedDraft, RagError> {
    let subject = "ITCy self-introduction";
    let (research_pack, pack_urls) = build_itcy_self_pack();
    let session_dir = begin_load_session_dir(tools, db_path, subject).await;
    checkpoint_building_pack(db_path, tools, subject, &research_pack, &pack_urls).await;
    if let Some(t) = tools {
        t.set_draft_policy(&pack_urls).await;
    }
    log_pipeline_banner("SELF-INTRO TWEET (X writer)");
    // Identity pack is static; writer must not tool-drift or essay-retry.
    let messages = vec![
        LlmMessage::system(self_tweet_system_prompt()),
        LlmMessage::user(self_user_message("x", instructions)),
    ];
    let (response, trace) = match router
        .complete_with_tools(TaskKind::Draft, &messages, None, 0)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            end_session_best_effort(
                tools,
                session_dir.as_ref(),
                &format!("self tweet failed: {e}"),
            )
            .await;
            return Err(e.into());
        }
    };
    let mut body = crate::sources::tweet::scrub_and_validate_tweet_body(&response.message.content)?;
    if tweet_body_exploded(&body) {
        warn!("self_tweet: writer dump coerced to tweet shape (no retry)");
        body = coerce_tweet_body(&body, subject);
    }
    let tweet_id = resolve_session_draft_id(tools, db_path).await;
    end_session_best_effort(
        tools,
        session_dir.as_ref(),
        &session_note(session_dir.as_ref()),
    )
    .await;
    Ok(assemble_tweet_draft(
        subject,
        &body,
        &trace,
        tweet_id,
        pack_urls,
        research_pack,
    ))
}

fn assemble_tweet_draft(
    subject: &str,
    body: &str,
    trace: &CompletionTrace,
    tweet_id: String,
    pack_urls: Vec<String>,
    research_pack: String,
) -> GroundedDraft {
    let body = strip_own_x_handle(body);
    let body = crate::sources::draft_url::strip_sources_section(&body);
    let body = ensure_tweet_cite_line(&body, in_tweet_publisher_url(&pack_urls));
    let mut link_options = pick_tweet_cite_options(&pack_urls, &body);
    body_ensure_repo_cite(&mut link_options);
    let body = crate::sources::tweet_footer::compose_self_intro_tweet_message(&body, &tweet_id);
    GroundedDraft {
        subject: subject.to_string(),
        body: with_disclosure(&body, trace),
        draft_id: tweet_id,
        model: format!("self-intro | tweet={}", trace.model_label()),
        tokens_in: trace.prompt_tokens,
        tokens_out: trace.completion_tokens,
        source_labels: pack_urls,
        link_options,
        research_pack,
    }
}

/// Ensures the `ITCy` public repo URL appears in the link options.
fn body_ensure_repo_cite(link_options: &mut Vec<String>) {
    const REPO: &str = "https://github.com/Interchouette-ITC/itcy";
    if !link_options.iter().any(|u| u.as_str() == REPO) {
        link_options.insert(0, REPO.to_string());
    }
}

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! LOAD pack then tweet writer. Does not overload the `LinkedIn` writer.

use crate::llm::agent::ToolProvider;
use crate::llm::client::{CompletionTrace, LlmMessage};
use crate::llm::clock::today_context_line;
use crate::llm::disclosure::with_disclosure;
use crate::llm::router::{FailoverRouter, TaskKind};
use crate::prompts::{
    tweet_pack_note, tweet_user_message, AI_CMO, CREATIVE_X, FORM_CRAFT_X, TWEET_SYSTEM_CORE,
    WHO_IS_WHO,
};
use crate::sources::embed::EmbedClient;
use crate::sources::rag::{
    begin_load_session_dir, checkpoint_building_pack, end_session_best_effort,
    resolve_session_draft_id, run_load_phase, GroundedDraft, RagError, MAX_TOOL_ROUNDS,
};
use crate::sources::tweet_footer::{
    aerate_tweet_commentary, compose_tweet_message, ensure_tweet_cite_line, extract_brief_cite,
    in_tweet_publisher_url, pick_tweet_cite_options, tweet_body_exploded,
};
use crate::sources::tweet_load::run_short_cite_load;
use crate::sources::url_hygiene::is_x_status_url;
use crate::tools::ItcyTools;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

fn tweet_system_prompt() -> String {
    format!(
        "{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}",
        today_context_line(),
        WHO_IS_WHO,
        AI_CMO,
        CREATIVE_X,
        FORM_CRAFT_X,
        TWEET_SYSTEM_CORE
    )
}

/// Load phase then tweet writer. Same pack as `LinkedIn` drafts; different prompt and footer.
///
/// # Errors
///
/// Returns a [`RagError`] variant for load/writer/LLM/store failure.
pub async fn build_grounded_tweet(
    router: &FailoverRouter,
    db_path: &Path,
    _embed: &dyn EmbedClient,
    subject: &str,
    tools: Option<&ItcyTools>,
) -> Result<GroundedDraft, RagError> {
    let session_dir = begin_load_session_dir(tools, db_path, subject).await;
    let tools_dyn: Option<&dyn ToolProvider> = tools.map(|t| t as &dyn ToolProvider);
    let prefer = extract_brief_cite(subject);
    let subject_https = prefer.is_some();

    let (mut research_pack, pack_urls, load_trace) = if let Some(url) = prefer.as_deref() {
        run_short_cite_load(subject, url, tools).await?
    } else {
        run_load_phase(router, subject, tools, tools_dyn, session_dir.as_ref()).await?
    };
    crate::sources::rag::apply_pack_handles(tools, subject, &mut research_pack);

    checkpoint_building_pack(db_path, tools, subject, &research_pack, &pack_urls).await;

    if let Some(t) = tools {
        if subject_https {
            t.set_subject_https_writer_policy().await;
        } else {
            t.set_draft_policy(&pack_urls).await;
        }
    }

    let (tweet_body, tweet_trace) = run_tweet_phase(
        router,
        subject,
        &research_pack,
        &pack_urls,
        subject_https,
        session_dir.as_ref(),
        tools,
    )
    .await?;

    info!(
        tweet_model = %tweet_trace.model_label(),
        "load_tweet: writer done"
    );

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

    let model = format!(
        "load={} | tweet={}",
        load_trace.model_label(),
        tweet_trace.model_label()
    );
    let tweet_body = ensure_tweet_handles_from_pack(tools, &tweet_body, &research_pack);
    let (body, link_options) =
        attach_tweet_cites(&tweet_body, &pack_urls, &tweet_id, prefer.as_deref());
    info!(
        tweet_id = %tweet_id,
        cites = link_options.len(),
        "load_tweet: tweet id + cites attached"
    );
    Ok(GroundedDraft {
        subject: subject.to_string(),
        body: with_disclosure(&body, &tweet_trace),
        draft_id: tweet_id,
        model,
        tokens_in: load_trace
            .prompt_tokens
            .saturating_add(tweet_trace.prompt_tokens),
        tokens_out: load_trace
            .completion_tokens
            .saturating_add(tweet_trace.completion_tokens),
        source_labels: if pack_urls.is_empty() {
            vec!["load+tweet (no verified cite)".into()]
        } else {
            pack_urls
        },
        link_options,
        research_pack,
    })
}

/// Writer-only tweet with a pre-filled Interchouette `ResearchPack` (skips LOAD / SERP).
///
/// # Errors
///
/// Returns a [`RagError`] variant for writer/LLM failure.
pub async fn build_grounded_tweet_from_pack(
    router: &FailoverRouter,
    db_path: &Path,
    subject: &str,
    research_pack: &str,
    pack_urls: &[String],
    tools: Option<&ItcyTools>,
) -> Result<GroundedDraft, RagError> {
    let session_dir = begin_load_session_dir(tools, db_path, subject).await;
    let urls: Vec<String> = pack_urls.to_vec();
    let mut research_pack = research_pack.to_string();
    crate::sources::rag::apply_pack_handles(tools, subject, &mut research_pack);
    checkpoint_building_pack(db_path, tools, subject, &research_pack, &urls).await;
    let prefer = extract_brief_cite(subject);
    let subject_https = prefer.is_some();
    if let Some(t) = tools {
        if subject_https {
            t.set_subject_https_writer_policy().await;
        } else {
            t.set_draft_policy(&urls).await;
        }
    }
    let (tweet_body, tweet_trace) = run_tweet_phase(
        router,
        subject,
        &research_pack,
        &urls,
        subject_https,
        session_dir.as_ref(),
        tools,
    )
    .await?;
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
    let tweet_body = ensure_tweet_handles_from_pack(tools, &tweet_body, &research_pack);
    let (body, link_options) = attach_tweet_cites(&tweet_body, &urls, &tweet_id, prefer.as_deref());
    Ok(GroundedDraft {
        subject: subject.to_string(),
        body: with_disclosure(&body, &tweet_trace),
        draft_id: tweet_id,
        model: format!("itc-pack | tweet={}", tweet_trace.model_label()),
        tokens_in: tweet_trace.prompt_tokens,
        tokens_out: tweet_trace.completion_tokens,
        source_labels: if urls.is_empty() {
            vec!["itc pack (no cite)".into()]
        } else {
            urls
        },
        link_options,
        research_pack,
    })
}

async fn run_tweet_phase(
    router: &FailoverRouter,
    subject: &str,
    research_pack: &str,
    pack_urls: &[String],
    subject_https: bool,
    session_dir: Option<&PathBuf>,
    tools: Option<&ItcyTools>,
) -> Result<(String, CompletionTrace), RagError> {
    crate::sources::rag::log_pipeline_banner("TWEET (writer)");
    // Subject already has an https URL: reuse it; writer must not browse/search/corpus away.
    let tools_dyn: Option<&dyn ToolProvider> = if subject_https {
        None
    } else {
        tools.map(|t| t as &dyn ToolProvider)
    };
    let user = tweet_user_message(
        research_pack,
        tweet_pack_note(pack_urls.is_empty(), subject_https),
        subject,
    );
    let messages = vec![
        LlmMessage::system(tweet_system_prompt()),
        LlmMessage::user(user.clone()),
    ];
    let (response, trace) = match router
        .complete_with_tools(TaskKind::Draft, &messages, tools_dyn, MAX_TOOL_ROUNDS)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            end_session_best_effort(tools, session_dir, &format!("tweet writer failed: {e}")).await;
            return Err(e.into());
        }
    };
    let body = scrub_tweet_body(&response.message.content);
    if !tweet_body_exploded(&body) {
        return Ok((body, trace));
    }
    warn!("load_tweet: writer dumped an essay; retrying tweet-only");
    let retry_user = format!(
        "{user}\n\nPREVIOUS OUTPUT WAS REJECTED (essay/ResearchPack). Output ONLY the tweet (3-4 aerated beats + tags + at most ONE bare https). No Sources. No headings."
    );
    let retry_messages = vec![
        LlmMessage::system(tweet_system_prompt()),
        LlmMessage::user(retry_user),
    ];
    let (response, trace) = match router
        .complete_with_tools(TaskKind::Draft, &retry_messages, tools_dyn, MAX_TOOL_ROUNDS)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            end_session_best_effort(
                tools,
                session_dir,
                &format!("tweet writer retry failed: {e}"),
            )
            .await;
            return Err(e.into());
        }
    };
    let body = scrub_tweet_body(&response.message.content);
    if tweet_body_exploded(&body) {
        return Err(RagError::NotATweet);
    }
    Ok((body, trace))
}

pub(crate) fn attach_tweet_cites(
    body: &str,
    pack_urls: &[String],
    tweet_id: &str,
    prefer: Option<&str>,
) -> (String, Vec<String>) {
    let body = crate::sources::draft_url::strip_sources_section(&aerate_tweet_commentary(body));
    let body = crate::sources::tweet_footer::strip_own_x_handle(&body);
    let mut link_options = pick_tweet_cite_options(pack_urls, &body);
    if let Some(cite) = prefer {
        crate::sources::draft_url::promote_link_option(&mut link_options, cite);
        // Subject X status: keep status options only (drop off-topic Brave publishers).
        if is_x_status_url(cite) {
            link_options.retain(|u| is_x_status_url(u));
            crate::sources::draft_url::promote_link_option(&mut link_options, cite);
        }
        // Same rule for X status and publisher: one https line in the tweet.
        let body = ensure_tweet_cite_line(&body, Some(cite));
        return (
            compose_tweet_message(&body, tweet_id, &link_options),
            link_options,
        );
    }
    let body = ensure_tweet_cite_line(&body, in_tweet_publisher_url(&link_options));
    (
        compose_tweet_message(&body, tweet_id, &link_options),
        link_options,
    )
}

pub(crate) fn scrub_tweet_body(raw: &str) -> String {
    let raw = crate::llm::sanitize_itcy_text(raw);
    let raw = crate::sources::draft_url::strip_sources_section(&raw);
    crate::sources::tweet_footer::strip_own_x_handle(&raw)
}

fn ensure_tweet_handles_from_pack(tools: Option<&ItcyTools>, body: &str, pack: &str) -> String {
    if let Some(t) = tools {
        let idx = t.handles_index();
        return crate::sources::handles::ensure_x_handle_from_pack(body, pack, &idx);
    }
    let owned = crate::sources::handles::load_handles().unwrap_or_default();
    crate::sources::handles::ensure_x_handle_from_pack(body, pack, &owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_x_cite_puts_x_url_in_body_no_sources() {
        let raw = "\
tinyboot fits in 1920 bytes.

https://neowin.net/news/windows-rootkit-now-included-with-coolclient-backdoor-targeting-governments

Sources: 
- https://x.com/AstraKernel/status/2088224406187413962
";
        let pack = vec![
            "https://x.com/AstraKernel/status/2088224406187413962".into(),
            "https://neowin.net/news/windows-rootkit-now-included-with-coolclient-backdoor-targeting-governments".into(),
        ];
        let prefer = "https://x.com/AstraKernel/status/2088224406187413962";
        let (out, opts) = attach_tweet_cites(raw, &pack, "TWEET-1", Some(prefer));
        assert!(!out.contains("Sources:"));
        assert!(!out.contains("neowin.net"));
        assert!(out.contains(&format!("1. {prefer}")));
        assert!(out.contains("Link: 1"));
        assert_eq!(opts[0], prefer);
        let api = crate::publish::tweet_text_for_api(&out);
        assert!(api.contains(prefer));
        assert!(!api.contains("neowin"));
        assert!(api.contains("tinyboot"));
    }
}

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! `/rework` on a `DRAFT-` id: rewrite an open draft with the same Draft ID.

use crate::bat::store::StoredDraft;
use crate::llm::agent::ToolProvider;
use crate::llm::client::LlmMessage;
use crate::llm::clock::today_context_line;
use crate::llm::disclosure::with_disclosure;
use crate::llm::router::{FailoverRouter, TaskKind};
use crate::llm::LlmError;
use crate::prompts::{
    draft_rework_refresh_user_message, draft_rework_user_message, rework_empty_pack,
    tweet_rework_commentary_empty, tweet_rework_commentary_exploded,
    tweet_rework_refresh_user_message, tweet_rework_user_message, TweetReworkUserArgs,
    DRAFT_REWORK_REFRESH_SYSTEM_CORE, DRAFT_REWORK_SYSTEM_CORE, TWEET_FARCE_SYSTEM_CORE,
    TWEET_REWORK_REFRESH_SYSTEM_CORE, TWEET_REWORK_SYSTEM_CORE, WHO_IS_WHO,
};
use crate::sources::draft_footer::{
    classify_rework_mode, compose_draft_message, ensure_primary_link_line,
    missing_required_quoted_spans, pick_link_options, rework_refresh_ban_phrases,
    rework_replacement_body, rework_required_quoted_spans, ReworkMode,
};
use crate::sources::draft_url::{extract_in_post_url, promote_link_option, set_single_in_post_url};
use crate::sources::handles::HandlesIndex;
use crate::sources::tweet_farce::{ensure_farce_mentions, stored_is_farce};
use crate::sources::tweet_footer::{
    aerate_tweet_commentary, coerce_tweet_body, compose_tweet_message, ensure_operator_https_lines,
    ensure_option, ensure_tweet_cite_line, in_tweet_publisher_url, operator_https_urls,
    pick_tweet_cite_options, strip_brand_org_at_handles, tweet_body_exploded,
};
use thiserror::Error;
use tracing::{info, warn};

fn rework_system_prompt() -> String {
    // Slim system on purpose: the full Creative/Form/write curriculum drowned operator
    // instructions and froze digest subject facts. Rework needs override first.
    format!(
        "{}\n\n{}\n\n{}",
        today_context_line(),
        WHO_IS_WHO,
        DRAFT_REWORK_SYSTEM_CORE
    )
}

fn draft_refresh_system_prompt() -> String {
    format!(
        "{}\n\n{}\n\n{}",
        today_context_line(),
        WHO_IS_WHO,
        DRAFT_REWORK_REFRESH_SYSTEM_CORE
    )
}

#[derive(Debug, Error)]
pub enum ReworkError {
    #[error("{0}")]
    Llm(#[from] LlmError),
    #[error("draft `{0}` is not open (status={1})")]
    NotOpen(String, String),
    #[error("rework did not produce a tweet (model dumped an essay); try shorter instructions")]
    NotATweet,
    #[error("{0}")]
    Operator(String),
}

/// Reworked draft payload (same shape as grounded draft for persist).
#[derive(Debug, Clone)]
pub struct ReworkedDraft {
    pub draft_id: String,
    pub subject: String,
    pub body: String,
    pub model: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub sources: Vec<String>,
    pub link_options: Vec<String>,
    pub research_pack: String,
}

/// Rewrite `stored` body using instructions + stored `ResearchPack` (draft route).
///
/// When the operator asks to search/browse (e.g. x402), tools may run like `/draft_about`.
///
/// # Errors
///
/// Returns a [`ReworkError`] variant when the draft is missing, not open, or LLM/store fails.
pub async fn rework_stored_draft(
    router: &FailoverRouter,
    stored: &StoredDraft,
    instructions: &str,
    tools: Option<&dyn ToolProvider>,
    handles: &HandlesIndex,
) -> Result<ReworkedDraft, ReworkError> {
    if stored.status != "open" {
        return Err(ReworkError::NotOpen(
            stored.draft_id.clone(),
            stored.status.clone(),
        ));
    }
    let current_url = extract_in_post_url(&stored.body);
    info!(
        draft_id = %stored.draft_id,
        in_post = ?current_url,
        instructions = %instructions.trim(),
        "rework: start"
    );
    match classify_rework_mode(instructions) {
        ReworkMode::Replace => {
            let body = rework_replacement_body(instructions);
            if body.trim().is_empty() {
                return Err(ReworkError::Operator(
                    "use text requires a draft body after the prefix".into(),
                ));
            }
            apply_draft_replacement(stored, body, handles, current_url, "").await
        }
        ReworkMode::Refresh => {
            rework_stored_draft_llm(router, stored, "", tools, handles, current_url, true).await
        }
        ReworkMode::Instruction => {
            let prior = crate::sources::draft_footer::draft_prose_for_rework(&stored.body);
            // Hard keywords like cite/quote: replace / is-handle must land in code.
            if crate::sources::draft_footer::rework_instructions_are_keyword_edits_only(
                instructions,
            ) {
                let edited =
                    crate::sources::draft_footer::apply_rework_keyword_edits(&prior, instructions);
                let missing = crate::sources::draft_footer::missing_rework_replace_outcomes(
                    &prior,
                    &edited,
                    instructions,
                );
                if !missing.is_empty() {
                    return Err(ReworkError::Operator(format!(
                        "could not apply replace/handle edits: {}",
                        missing.join("; ")
                    )));
                }
                let out =
                    apply_draft_replacement(stored, &edited, handles, current_url, instructions)
                        .await?;
                let after = crate::sources::draft_footer::draft_prose_for_rework(&out.body);
                let missing = crate::sources::draft_footer::missing_rework_replace_outcomes(
                    &prior,
                    &after,
                    instructions,
                );
                if !missing.is_empty() {
                    return Err(ReworkError::Operator(format!(
                        "could not apply replace/handle edits: {}",
                        missing.join("; ")
                    )));
                }
                return Ok(out);
            }
            let mut out = rework_stored_draft_llm(
                router,
                stored,
                instructions,
                tools,
                handles,
                current_url.clone(),
                false,
            )
            .await?;
            // Re-apply keyword edits after the model so Replace / is-handle cannot be ignored.
            let prose = crate::sources::draft_footer::draft_prose_for_rework(&out.body);
            let edited =
                crate::sources::draft_footer::apply_rework_keyword_edits(&prose, instructions);
            if edited != prose {
                out = apply_draft_replacement(
                    stored,
                    &edited,
                    handles,
                    current_url.clone(),
                    instructions,
                )
                .await?;
            }
            let prose_after = crate::sources::draft_footer::draft_prose_for_rework(&out.body);
            let missing = crate::sources::draft_footer::missing_rework_replace_outcomes(
                &prior,
                &prose_after,
                instructions,
            );
            if !missing.is_empty() {
                return Err(ReworkError::Operator(format!(
                    "writer did not apply replace/handle instructions: {}",
                    missing.join("; ")
                )));
            }
            out = enforce_required_quotes_draft(
                router,
                stored,
                instructions,
                tools,
                handles,
                current_url,
                out,
            )
            .await?;
            Ok(out)
        }
    }
}

async fn enforce_required_quotes_draft(
    router: &FailoverRouter,
    stored: &StoredDraft,
    instructions: &str,
    tools: Option<&dyn ToolProvider>,
    handles: &HandlesIndex,
    current_url: Option<String>,
    first: ReworkedDraft,
) -> Result<ReworkedDraft, ReworkError> {
    let required = rework_required_quoted_spans(instructions);
    if required.is_empty() {
        return Ok(first);
    }
    let prose = crate::sources::draft_footer::draft_prose_for_rework(&first.body);
    let missing = missing_required_quoted_spans(&prose, &required);
    if missing.is_empty() {
        return Ok(first);
    }
    let louder = format!(
        "{instructions}\n\n{}",
        crate::sources::draft_footer::louder_required_quotes_note(&missing)
    );
    let second =
        rework_stored_draft_llm(router, stored, &louder, tools, handles, current_url, false)
            .await?;
    let prose2 = crate::sources::draft_footer::draft_prose_for_rework(&second.body);
    let still = missing_required_quoted_spans(&prose2, &required);
    if still.is_empty() {
        return Ok(second);
    }
    Err(ReworkError::Operator(
        crate::sources::draft_footer::missing_quotes_operator_error(&still),
    ))
}

fn format_ban_block(banned: &[String]) -> String {
    if banned.is_empty() {
        return "(none)".into();
    }
    banned
        .iter()
        .map(|p| format!("- {p}"))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn rework_stored_draft_llm(
    router: &FailoverRouter,
    stored: &StoredDraft,
    instructions: &str,
    tools: Option<&dyn ToolProvider>,
    handles: &HandlesIndex,
    current_url: Option<String>,
    refresh: bool,
) -> Result<ReworkedDraft, ReworkError> {
    let mut pack = if stored.research_pack.trim().is_empty() {
        rework_empty_pack(&stored.subject)
    } else {
        stored.research_pack.clone()
    };
    let url_lock = current_url
        .as_deref()
        .unwrap_or("(none - pick one pack URL)");
    let prose = crate::llm::sanitize_itcy_text(
        &crate::sources::draft_footer::draft_prose_for_rework(&stored.body),
    );
    let brief_for_handles = format!("{}\n{instructions}\n{prose}", stored.subject);
    crate::sources::handles::apply_brief_handles_to_pack(&mut pack, &brief_for_handles, handles);
    let (system, user) = if refresh {
        let banned = rework_refresh_ban_phrases(&prose);
        let ban_block = format_ban_block(&banned);
        (
            draft_refresh_system_prompt(),
            draft_rework_refresh_user_message(
                &stored.draft_id,
                &stored.subject,
                &pack,
                &prose,
                url_lock,
                &ban_block,
            ),
        )
    } else {
        (
            rework_system_prompt(),
            draft_rework_user_message(
                instructions,
                &stored.draft_id,
                &stored.subject,
                &pack,
                &prose,
                url_lock,
            ),
        )
    };
    let messages = vec![LlmMessage::system(system), LlmMessage::user(user)];
    let (response, trace) = router
        .complete_with_tools(TaskKind::Draft, &messages, tools, 6)
        .await?;
    let mut body = crate::llm::sanitize_itcy_text(response.message.content.trim());
    body = crate::sources::draft_footer::strip_leading_page_title_lede(&body);
    body = crate::sources::draft_footer::strip_leading_cite_instruction(&body);
    body = crate::sources::draft_footer::strip_rework_quoted_removals(&body, instructions);
    body = crate::sources::draft_footer::aerate_linkedin_draft(&body);
    let pack_urls = stored.sources.clone();
    let mut link_options = if stored.link_options.is_empty() {
        pick_link_options(&pack_urls, &body)
    } else {
        stored.link_options.clone()
    };
    if link_options.is_empty() {
        link_options = pick_link_options(&pack_urls, &body);
    }
    let primary = current_url
        .clone()
        .or_else(|| link_options.first().cloned())
        .unwrap_or_default();
    if primary.is_empty() {
        body = ensure_primary_link_line(&body, link_options.first().map(String::as_str));
    } else {
        promote_link_option(&mut link_options, &primary);
        body = set_single_in_post_url(&body, &primary);
    }
    (body, link_options) =
        crate::sources::publisher_url::finalize_reachable_link_options_from_pool(
            &body,
            link_options,
            &pack_urls,
        )
        .await;
    body = strip_leading_draft_id(&body);
    body = crate::sources::handles::ensure_linkedin_brand_mention(&body);
    body = crate::sources::handles::ensure_linkedin_handle_from_pack(&body, &pack, handles);
    // Operator `replace` / `is handle` win over pack handle inject.
    body = crate::sources::draft_footer::apply_rework_keyword_edits(&body, instructions);
    let body = compose_draft_message(&body, &stored.draft_id, &link_options);
    let body = with_disclosure(&body, &trace);
    info!(
        draft_id = %stored.draft_id,
        in_post = %primary,
        option1 = ?link_options.first(),
        "rework: done (single in-post URL enforced)"
    );
    Ok(ReworkedDraft {
        draft_id: stored.draft_id.clone(),
        subject: stored.subject.clone(),
        body,
        model: format!("rework={}", trace.model_label()),
        tokens_in: trace.prompt_tokens,
        tokens_out: trace.completion_tokens,
        sources: pack_urls,
        link_options,
        research_pack: pack,
    })
}

/// Apply a pasted full `LinkedIn` draft as `/rework` replace (no LLM).
///
/// `keyword_instructions` are operator `/rework` keywords (`replace` / `is handle`).
/// Re-applied after handle inject so pack `@slug` cannot undo an explicit plain-name replace.
async fn apply_draft_replacement(
    stored: &StoredDraft,
    paste: &str,
    handles: &HandlesIndex,
    current_url: Option<String>,
    keyword_instructions: &str,
) -> Result<ReworkedDraft, ReworkError> {
    let mut pack = if stored.research_pack.trim().is_empty() {
        rework_empty_pack(&stored.subject)
    } else {
        stored.research_pack.clone()
    };
    let brief_for_handles = format!("{}\n{paste}", stored.subject);
    crate::sources::handles::apply_brief_handles_to_pack(&mut pack, &brief_for_handles, handles);
    let mut body = crate::llm::sanitize_itcy_text(paste.trim());
    body = crate::sources::draft_footer::strip_leading_page_title_lede(&body);
    body = crate::sources::draft_footer::strip_leading_cite_instruction(&body);
    body = crate::sources::draft_footer::aerate_linkedin_draft(&body);
    let pack_urls = stored.sources.clone();
    let mut link_options = if stored.link_options.is_empty() {
        pick_link_options(&pack_urls, &body)
    } else {
        stored.link_options.clone()
    };
    if link_options.is_empty() {
        link_options = pick_link_options(&pack_urls, &body);
    }
    let primary = current_url
        .clone()
        .or_else(|| link_options.first().cloned())
        .unwrap_or_default();
    if primary.is_empty() {
        body = ensure_primary_link_line(&body, link_options.first().map(String::as_str));
    } else {
        promote_link_option(&mut link_options, &primary);
        body = set_single_in_post_url(&body, &primary);
    }
    (body, link_options) =
        crate::sources::publisher_url::finalize_reachable_link_options_from_pool(
            &body,
            link_options,
            &pack_urls,
        )
        .await;
    body = strip_leading_draft_id(&body);
    body = crate::sources::handles::ensure_linkedin_brand_mention(&body);
    body = crate::sources::handles::ensure_linkedin_handle_from_pack(&body, &pack, handles);
    body = crate::sources::draft_footer::apply_rework_keyword_edits(&body, keyword_instructions);
    let body = compose_draft_message(&body, &stored.draft_id, &link_options);
    let trace = crate::llm::client::CompletionTrace {
        provider: "operator".into(),
        model: "rework-replace".into(),
        prompt_tokens: 0,
        completion_tokens: 0,
    };
    let body = with_disclosure(&body, &trace);
    info!(
        draft_id = %stored.draft_id,
        in_post = %primary,
        "rework: replace applied (no LLM)"
    );
    Ok(ReworkedDraft {
        draft_id: stored.draft_id.clone(),
        subject: stored.subject.clone(),
        body,
        model: format!("rework={}", trace.model_label()),
        tokens_in: 0,
        tokens_out: 0,
        sources: pack_urls,
        link_options,
        research_pack: pack,
    })
}

fn tweet_rework_system_prompt(farce: bool) -> String {
    // Slim system on purpose: the full Creative/Form/write curriculum drowned operator
    // instructions and froze digest subject facts. Rework needs override first.
    if farce {
        format!(
            "{}\n\n{}\n\n{}\n\n{}",
            today_context_line(),
            WHO_IS_WHO,
            TWEET_REWORK_SYSTEM_CORE,
            TWEET_FARCE_SYSTEM_CORE
        )
    } else {
        format!(
            "{}\n\n{}\n\n{}",
            today_context_line(),
            WHO_IS_WHO,
            TWEET_REWORK_SYSTEM_CORE
        )
    }
}

/// Fold operator rework into the stored subject (audit trail for the next turn).
#[must_use]
pub fn merge_rework_subject(prior: &str, instructions: &str) -> String {
    let prior = prior.trim();
    let inst = sanitize_rework_instructions(instructions);
    if inst.is_empty() {
        return prior.to_string();
    }
    if prior.is_empty() {
        return inst;
    }
    format!("{prior}\n\n[operator rework] {inst}")
}

/// Strip paste noise (`|` from ack mimic, leading commas, Slack `*bold*`) from `/rework` instructions.
#[must_use]
pub fn sanitize_rework_instructions(raw: &str) -> String {
    let mut t = raw.trim();
    while let Some(rest) = t.strip_prefix('|').or_else(|| t.strip_prefix(',')) {
        t = rest.trim_start();
    }
    t.replace('*', "").trim().to_string()
}

fn tweet_rework_needs_tools(instructions: &str) -> bool {
    let t = instructions.to_ascii_lowercase();
    t.contains("web_search") || t.contains("browse_url")
}

#[cfg(all(test, itcy_kitchen_prompts))]
fn tweet_rework_user_prompt(
    tweet_id: &str,
    subject: &str,
    pack: &str,
    stored_body: &str,
    cite: &str,
    instructions: &str,
    farce: bool,
) -> String {
    let commentary_raw = crate::publish::tweet_text_for_api(stored_body);
    let commentary = if tweet_body_exploded(&commentary_raw) {
        tweet_rework_commentary_exploded().to_string()
    } else if commentary_raw.is_empty() {
        tweet_rework_commentary_empty().to_string()
    } else {
        commentary_raw
    };
    tweet_rework_user_message(&TweetReworkUserArgs {
        instructions,
        id: tweet_id,
        subject,
        commentary: &commentary,
        cite,
        pack,
        farce,
        needs_tools: !farce && tweet_rework_needs_tools(instructions),
    })
}

fn finalize_rework_tweet_output(
    mut body: String,
    stored: &StoredDraft,
    pack_urls: &[String],
    current: &str,
    instructions: &str,
    farce: bool,
) -> (String, Vec<String>) {
    let mut link_options = if stored.link_options.is_empty() {
        pick_tweet_cite_options(pack_urls, &body)
    } else {
        stored.link_options.clone()
    };
    if link_options.is_empty() {
        link_options = pick_tweet_cite_options(pack_urls, &body);
    }
    if !current.is_empty() {
        ensure_option(&mut link_options, current);
    }
    let mut required = operator_https_urls(&stored.subject);
    for u in operator_https_urls(instructions) {
        if !required
            .iter()
            .any(|x| crate::sources::url_hygiene::same_publisher_url(x, &u))
        {
            required.push(u);
        }
    }
    if !current.is_empty()
        && !required
            .iter()
            .any(|x| crate::sources::url_hygiene::same_publisher_url(x, current))
    {
        required.insert(0, current.to_string());
    }
    for u in &required {
        ensure_option(&mut link_options, u);
    }
    if farce && current.is_empty() {
        link_options.clear();
    }
    body = aerate_tweet_commentary(&body);
    body = crate::sources::draft_url::strip_sources_section(&body);
    let has_non_x = required
        .iter()
        .any(|u| !crate::sources::url_hygiene::is_x_status_url(u));
    if has_non_x {
        body = strip_brand_org_at_handles(&body);
    }
    let in_tweet = if farce && current.is_empty() {
        None
    } else if current.is_empty() {
        extract_in_post_url(&body)
            .or_else(|| in_tweet_publisher_url(&link_options).map(str::to_string))
    } else {
        Some(current.to_string())
    };
    body = if farce && current.is_empty() {
        ensure_tweet_cite_line(&body, None)
    } else {
        ensure_operator_https_lines(&body, &required, in_tweet.as_deref())
    };
    body = strip_leading_draft_id(&body);
    body = strip_leading_tweet_id(&body);
    (body, link_options)
}

/// Rewrite a stored tweet (same id) using tweet writer rules.
///
/// # Errors
///
/// Returns a [`ReworkError`] variant when the tweet is missing, not open, or LLM/store fails.
pub async fn rework_stored_tweet(
    router: &FailoverRouter,
    stored: &StoredDraft,
    instructions: &str,
    tools: Option<&dyn ToolProvider>,
    handles: &HandlesIndex,
) -> Result<ReworkedDraft, ReworkError> {
    if stored.status != "open" {
        return Err(ReworkError::NotOpen(
            stored.draft_id.clone(),
            stored.status.clone(),
        ));
    }
    let instructions = sanitize_rework_instructions(instructions);
    let instructions = instructions.as_str();
    let pack_for_farce = if stored.research_pack.trim().is_empty() {
        rework_empty_pack(&stored.subject)
    } else {
        stored.research_pack.clone()
    };
    let current = stored.link_options.first().cloned().unwrap_or_else(|| {
        crate::sources::draft_url::extract_in_post_url(&stored.body).unwrap_or_default()
    });
    let farce = stored_is_farce(&pack_for_farce, &stored.body);
    info!(
        tweet_id = %stored.draft_id,
        instructions = %instructions,
        "rework: tweet start"
    );
    match classify_rework_mode(instructions) {
        ReworkMode::Replace => {
            let body = rework_replacement_body(instructions);
            if body.trim().is_empty() {
                return Err(ReworkError::Operator(
                    "use text requires a tweet body after the prefix".into(),
                ));
            }
            Ok(apply_tweet_replacement(
                stored, body, handles, &current, farce, "",
            ))
        }
        ReworkMode::Refresh => {
            rework_stored_tweet_llm(
                router,
                TweetReworkLlmArgs {
                    stored,
                    instructions: "",
                    tools,
                    handles,
                    current: &current,
                    farce,
                    refresh: true,
                },
            )
            .await
        }
        ReworkMode::Instruction => {
            rework_tweet_instruction(
                router,
                stored,
                instructions,
                tools,
                handles,
                &current,
                farce,
            )
            .await
        }
    }
}

async fn rework_tweet_instruction(
    router: &FailoverRouter,
    stored: &StoredDraft,
    instructions: &str,
    tools: Option<&dyn ToolProvider>,
    handles: &HandlesIndex,
    current: &str,
    farce: bool,
) -> Result<ReworkedDraft, ReworkError> {
    let prior = crate::publish::tweet_text_for_api(&stored.body);
    if crate::sources::draft_footer::rework_instructions_are_keyword_edits_only(instructions) {
        let edited = crate::sources::draft_footer::apply_rework_keyword_edits(&prior, instructions);
        let missing = crate::sources::draft_footer::missing_rework_replace_outcomes(
            &prior,
            &edited,
            instructions,
        );
        if !missing.is_empty() {
            return Err(ReworkError::Operator(format!(
                "could not apply replace/handle edits: {}",
                missing.join("; ")
            )));
        }
        let out = apply_tweet_replacement(stored, &edited, handles, current, farce, instructions);
        let after = crate::publish::tweet_text_for_api(&out.body);
        let missing = crate::sources::draft_footer::missing_rework_replace_outcomes(
            &prior,
            &after,
            instructions,
        );
        if !missing.is_empty() {
            return Err(ReworkError::Operator(format!(
                "could not apply replace/handle edits: {}",
                missing.join("; ")
            )));
        }
        return Ok(out);
    }
    let llm_args = TweetReworkLlmArgs {
        stored,
        instructions,
        tools,
        handles,
        current,
        farce,
        refresh: false,
    };
    let mut out = rework_stored_tweet_llm(router, llm_args).await?;
    let prose = crate::publish::tweet_text_for_api(&out.body);
    let edited = crate::sources::draft_footer::apply_rework_keyword_edits(&prose, instructions);
    if edited != prose {
        out = apply_tweet_replacement(stored, &edited, handles, current, farce, instructions);
    }
    let after = crate::publish::tweet_text_for_api(&out.body);
    let missing =
        crate::sources::draft_footer::missing_rework_replace_outcomes(&prior, &after, instructions);
    if !missing.is_empty() {
        return Err(ReworkError::Operator(format!(
            "writer did not apply replace/handle instructions: {}",
            missing.join("; ")
        )));
    }
    enforce_required_quotes_tweet(
        router,
        TweetReworkLlmArgs {
            stored,
            instructions,
            tools,
            handles,
            current,
            farce,
            refresh: false,
        },
        out,
    )
    .await
}

async fn enforce_required_quotes_tweet(
    router: &FailoverRouter,
    args: TweetReworkLlmArgs<'_>,
    first: ReworkedDraft,
) -> Result<ReworkedDraft, ReworkError> {
    let required = rework_required_quoted_spans(args.instructions);
    if required.is_empty() {
        return Ok(first);
    }
    let commentary = crate::publish::tweet_text_for_api(&first.body);
    let missing = missing_required_quoted_spans(&commentary, &required);
    if missing.is_empty() {
        return Ok(first);
    }
    let louder = format!(
        "{}\n\n{}",
        args.instructions,
        crate::sources::draft_footer::louder_required_quotes_note(&missing)
    );
    let second = rework_stored_tweet_llm(
        router,
        TweetReworkLlmArgs {
            instructions: &louder,
            ..args
        },
    )
    .await?;
    let commentary2 = crate::publish::tweet_text_for_api(&second.body);
    let still = missing_required_quoted_spans(&commentary2, &required);
    if still.is_empty() {
        return Ok(second);
    }
    Err(ReworkError::Operator(
        crate::sources::draft_footer::missing_quotes_operator_error(&still),
    ))
}

fn apply_tweet_replacement(
    stored: &StoredDraft,
    paste: &str,
    handles: &HandlesIndex,
    current: &str,
    farce: bool,
    keyword_instructions: &str,
) -> ReworkedDraft {
    let mut pack = if stored.research_pack.trim().is_empty() {
        rework_empty_pack(&stored.subject)
    } else {
        stored.research_pack.clone()
    };
    crate::sources::handles::apply_brief_handles_to_pack(
        &mut pack,
        &format!("{}\n{paste}", stored.subject),
        handles,
    );
    let mut body = scrub_rework_tweet_body(paste);
    if farce {
        body = ensure_farce_mentions(&body);
    }
    body = crate::sources::handles::ensure_x_handle_from_pack(&body, &pack, handles);
    // Operator `replace` / `is handle` win over pack handle inject (e.g. Wasmer ← @wasmerio).
    body = crate::sources::draft_footer::apply_rework_keyword_edits(&body, keyword_instructions);
    let pack_urls = stored.sources.clone();
    let (body, link_options) =
        finalize_rework_tweet_output(body, stored, &pack_urls, current, paste, farce);
    let body = compose_tweet_message(&body, &stored.draft_id, &link_options);
    let trace = crate::llm::client::CompletionTrace {
        provider: "operator".into(),
        model: "rework-replace".into(),
        prompt_tokens: 0,
        completion_tokens: 0,
    };
    let body = with_disclosure(&body, &trace);
    info!(
        tweet_id = %stored.draft_id,
        "rework: tweet replace applied (no LLM)"
    );
    ReworkedDraft {
        draft_id: stored.draft_id.clone(),
        subject: stored.subject.clone(),
        body,
        model: format!("rework={}", trace.model_label()),
        tokens_in: 0,
        tokens_out: 0,
        sources: pack_urls,
        link_options,
        research_pack: pack,
    }
}

#[derive(Clone, Copy)]
struct TweetReworkLlmArgs<'a> {
    stored: &'a StoredDraft,
    instructions: &'a str,
    tools: Option<&'a dyn ToolProvider>,
    handles: &'a HandlesIndex,
    current: &'a str,
    farce: bool,
    refresh: bool,
}

async fn rework_stored_tweet_llm(
    router: &FailoverRouter,
    args: TweetReworkLlmArgs<'_>,
) -> Result<ReworkedDraft, ReworkError> {
    let TweetReworkLlmArgs {
        stored,
        instructions,
        tools,
        handles,
        current,
        farce,
        refresh,
    } = args;
    let mut pack = if stored.research_pack.trim().is_empty() {
        rework_empty_pack(&stored.subject)
    } else {
        stored.research_pack.clone()
    };
    crate::sources::handles::apply_brief_handles_to_pack(
        &mut pack,
        &format!("{}\n{instructions}", stored.subject),
        handles,
    );
    let cite = if current.is_empty() {
        "(none)"
    } else {
        current
    };
    let commentary_raw = crate::publish::tweet_text_for_api(&stored.body);
    let commentary = if tweet_body_exploded(&commentary_raw) {
        tweet_rework_commentary_exploded().to_string()
    } else if commentary_raw.is_empty() {
        tweet_rework_commentary_empty().to_string()
    } else {
        commentary_raw
    };
    let (system, user) = if refresh && !farce {
        let banned = rework_refresh_ban_phrases(&commentary);
        let ban_block = format_ban_block(&banned);
        (
            tweet_refresh_system_prompt(),
            tweet_rework_refresh_user_message(
                &stored.draft_id,
                stored.subject.as_str(),
                &pack,
                &commentary,
                cite,
                &ban_block,
            ),
        )
    } else {
        (
            tweet_rework_system_prompt(farce),
            tweet_rework_user_message(&TweetReworkUserArgs {
                instructions,
                id: &stored.draft_id,
                subject: stored.subject.as_str(),
                commentary: &commentary,
                cite,
                pack: &pack,
                farce,
                needs_tools: !farce && tweet_rework_needs_tools(instructions),
            }),
        )
    };
    let (mut body, trace) =
        run_tweet_rework_llm(router, system, user, instructions, tools, farce).await?;
    if farce {
        body = ensure_farce_mentions(&body);
    }
    body = crate::sources::handles::ensure_x_handle_from_pack(&body, &pack, handles);
    body = crate::sources::draft_footer::apply_rework_keyword_edits(&body, instructions);
    let pack_urls = stored.sources.clone();
    let (body, link_options) =
        finalize_rework_tweet_output(body, stored, &pack_urls, current, instructions, farce);
    let body = compose_tweet_message(&body, &stored.draft_id, &link_options);
    let body = with_disclosure(&body, &trace);
    Ok(ReworkedDraft {
        draft_id: stored.draft_id.clone(),
        subject: merge_rework_subject(&stored.subject, instructions),
        body,
        model: format!("rework={}", trace.model_label()),
        tokens_in: trace.prompt_tokens,
        tokens_out: trace.completion_tokens,
        sources: pack_urls,
        link_options,
        research_pack: pack,
    })
}

fn tweet_refresh_system_prompt() -> String {
    format!(
        "{}\n\n{}\n\n{}",
        today_context_line(),
        WHO_IS_WHO,
        TWEET_REWORK_REFRESH_SYSTEM_CORE
    )
}

async fn run_tweet_rework_llm(
    router: &FailoverRouter,
    system: String,
    user: String,
    instructions: &str,
    tools: Option<&dyn ToolProvider>,
    farce: bool,
) -> Result<(String, crate::llm::client::CompletionTrace), ReworkError> {
    let tools_for_call = if farce || !tweet_rework_needs_tools(instructions) {
        None
    } else {
        tools
    };
    let messages = vec![LlmMessage::system(system), LlmMessage::user(user)];
    let (response, trace) = router
        .complete_with_tools(TaskKind::Draft, &messages, tools_for_call, 2)
        .await?;
    let body = scrub_rework_tweet_body(&response.message.content);
    if tweet_body_exploded(&body) {
        warn!("rework: tweet writer dump coerced to tweet shape (no retry)");
        return Ok((coerce_tweet_body(&body, "rework"), trace));
    }
    Ok((body, trace))
}

fn scrub_rework_tweet_body(raw: &str) -> String {
    crate::sources::tweet::scrub_tweet_body(raw)
}

fn strip_leading_draft_id(body: &str) -> String {
    let mut lines = body.lines().peekable();
    if let Some(first) = lines.peek() {
        if first.trim().starts_with("Draft ID:") {
            let _ = lines.next();
            while lines.peek().is_some_and(|l| l.trim().is_empty()) {
                let _ = lines.next();
            }
            return lines.collect::<Vec<_>>().join("\n");
        }
    }
    body.to_string()
}

fn strip_leading_tweet_id(body: &str) -> String {
    let mut lines = body.lines().peekable();
    if let Some(first) = lines.peek() {
        if first.trim().starts_with("Tweet ID:") {
            let _ = lines.next();
            while lines.peek().is_some_and(|l| l.trim().is_empty()) {
                let _ = lines.next();
            }
            return lines.collect::<Vec<_>>().join("\n");
        }
    }
    body.to_string()
}

#[cfg(all(test, itcy_kitchen_prompts))]
mod tests {
    use super::*;
    use crate::prompts::{AI_CMO, FORM_CRAFT_X};

    #[test]
    fn rework_always_includes_previous_tweet() {
        let prior = "🚀 No mcp:// yet, but ITC is about to drop the protocol on mcpare.com.\n\nwe're building it.\n\nhttps://aaif.io/a\n";
        let user = tweet_rework_user_prompt(
            "TWEET-1",
            "mcp:// there is no mcp:// protocol right now",
            "## ResearchPack\n",
            prior,
            "https://aaif.io/a",
            "mcp:// is already being proposed as a URI scheme; ITC is only dreaming about contributing for mcpare.com",
            false,
        );
        assert!(
            user.contains("No mcp:// yet"),
            "previous tweet must stay in the prompt: {user}"
        );
        assert!(user.contains("we're building"));
        assert!(user.contains("already being proposed"));
        assert!(!user.contains("previous draft omitted"));
        let polish = tweet_rework_user_prompt(
            "TWEET-1",
            "rust",
            "## ResearchPack\n",
            "plain tweet about rust\n\nhttps://itsfoss.com/a",
            "https://itsfoss.com/a",
            "add emojis",
            false,
        );
        assert!(polish.contains("plain tweet about rust"));
    }

    #[test]
    fn tweet_rework_puts_operator_instructions_first_and_strips_footer() {
        let stored = "Tweet ID: TWEET-20260814-000004\n\n\
I manage Interchouette ITC's LinkedIn page.\n\n\
https://github.com/Interchouette\n\n\
Cite = option 1 (publisher URL in body).\n\
1. https://github.com/Interchouette\n";
        let user = tweet_rework_user_prompt(
            "TWEET-20260814-000004",
            "itc intro",
            "## ResearchPack\n",
            stored,
            "https://github.com/Interchouette",
            "I manage the company X account as a disclosed AI CMO",
            false,
        );
        assert!(user.contains("OPERATOR REWORK INSTRUCTIONS"));
        assert!(user.contains("company X account"));
        assert!(
            user.contains("I manage Interchouette ITC's LinkedIn page."),
            "previous tweet prose must stay in the prompt: {user}"
        );
        assert!(!user.contains("previous draft omitted"));
        assert!(user.contains("X tweet"));
        assert!(user.contains("@Interchouette"));
        assert!(!user.contains("Cite = option"));
        assert!(user.contains("HARD OUTPUT"));
        assert!(!user.contains("ResearchPack (operator asked"));
        assert!(
            !user.contains("Subject (locked)"),
            "locked subject freezes wrong digest facts: {user}"
        );
        assert!(user.contains("NOT a fact lock") || user.contains("highest priority"));
    }

    #[test]
    fn tweet_rework_system_is_slim_override_not_write_subject_lock() {
        let sys = tweet_rework_system_prompt(false);
        assert!(sys.contains("OPERATOR OVERRIDE"));
        assert!(sys.to_ascii_lowercase().contains("fact lock"));
        assert!(
            !sys.contains("SUBJECT LOCK (hard on write") && !sys.contains("SUBJECT LOCK (hard)\n"),
            "write-time SUBJECT LOCK must not drive rework"
        );
        assert!(
            !sys.contains("CREATIVE MANDATE") && !sys.contains("X CREATIVE STUDIO"),
            "full creative curriculum must not drown rework instructions"
        );
        assert!(
            sys.len() < 12_000,
            "rework system should stay slim, got {} chars",
            sys.len()
        );
    }

    #[test]
    fn merge_rework_subject_appends_operator_notes() {
        let s = merge_rework_subject(
            "mcp:// there is no mcp:// protocol right now",
            "mcp:// is already proposed as a URI scheme for MCP discovery; ITC is only dreaming about contributing for mcpare.com",
        );
        assert!(s.contains("already proposed"));
        assert!(s.contains("contributing"));
        assert!(s.contains("there is no mcp://"));
        assert!(s.contains("[operator rework]"));
        let short = merge_rework_subject("rust policy", "add emojis");
        assert!(short.contains("rust policy"));
        assert!(short.contains("[operator rework]"));
    }

    #[test]
    fn long_rework_note_keeps_previous_tweet_in_prompt() {
        let instructions = "remove Comment \"obscura\" for the GitHub link.";
        let prior = "📜 A solo dev built a Rust browser.\n\n🦀 Obscura’s footprint crushes headless Chrome.\n\nhttps://x.com/thevibefounder/status/1\n";
        let user = tweet_rework_user_prompt(
            "TWEET-1",
            "Obscura Rust browser | cite https://x.com/thevibefounder/status/1",
            "## ResearchPack\n",
            prior,
            "https://x.com/thevibefounder/status/1",
            instructions,
            false,
        );
        assert!(user.contains("Obscura"));
        assert!(!user.contains("previous draft omitted"));
        let merged = merge_rework_subject(
            "Obscura Rust browser | cite https://x.com/thevibefounder/status/1",
            instructions,
        );
        assert!(merged.contains("Obscura Rust browser"));
        assert!(merged.contains("[operator rework]"));
    }

    #[test]
    fn tone_word_fix_keeps_previous_tweet_in_prompt() {
        let instructions = "58% faster than AWS SDK is too agressive say 58% More Throughput";
        let prior =
            "🦉 Scylla's driver is 58% faster than AWS SDK.\n\nhttps://www.scylladb.com/a\n";
        let user = tweet_rework_user_prompt(
            "TWEET-20260828-000092",
            "ScyllaDB Rust driver",
            "## ResearchPack\n",
            prior,
            "https://www.scylladb.com/a",
            instructions,
            false,
        );
        assert!(user.contains("58% faster than AWS SDK"));
        assert!(!user.contains("previous draft omitted"));
        let merged = merge_rework_subject("ScyllaDB Rust driver", instructions);
        assert!(merged.contains("ScyllaDB Rust driver"));
        assert!(merged.contains("[operator rework]"));
    }

    #[test]
    fn sanitize_strips_leading_pipe_from_ack_mimic() {
        assert_eq!(
            sanitize_rework_instructions("| interestingly, *mcp://* is proposed"),
            "interestingly, mcp:// is proposed"
        );
        assert_eq!(
            sanitize_rework_instructions(",  make shorter"),
            "make shorter"
        );
    }

    #[test]
    fn tweet_rework_always_applies_form_craft() {
        let with_emoji_ask = tweet_rework_user_prompt(
            "TWEET-1",
            "rust llm",
            "## ResearchPack\n",
            "plain tweet\n\nhttps://itsfoss.com/a",
            "https://itsfoss.com/a",
            "add emojis !!!!",
            false,
        );
        let shorter = tweet_rework_user_prompt(
            "TWEET-1",
            "rust llm",
            "## ResearchPack\n",
            "plain tweet\n\nhttps://itsfoss.com/a",
            "https://itsfoss.com/a",
            "make it shorter",
            false,
        );
        assert!(FORM_CRAFT_X.contains("EMOJI AS LANGUAGE"));
        assert!(FORM_CRAFT_X.contains("compulsory"));
        assert!(FORM_CRAFT_X.contains('🦉') && FORM_CRAFT_X.contains('🦀'));
        assert!(!FORM_CRAFT_X.contains("🦉 (ITCy)"));
        assert!(WHO_IS_WHO.contains('🦉') && WHO_IS_WHO.contains('🦀'));
        assert!(AI_CMO.contains("AI CMO") && AI_CMO.contains("/propose_tweet"));
        for user in [&with_emoji_ask, &shorter] {
            assert!(user.contains("SCOPE:"));
            assert!(
                user.contains("Form craft")
                    || user.contains("AI CMO")
                    || user.contains("Emoji compulsory")
                    || user.contains("emoji compulsory")
                    || user.contains("1-3")
            );
            assert!(
                user.contains("only improve emoji")
                    || user.contains("only emoji")
                    || user.contains("keep / restore prose")
            );
            assert!(!user.contains("operator asked for emoji"));
            assert!(!user.contains("Prefer 🦉 (ITCy) and 🦀 (Rust)"));
        }
    }

    #[test]
    fn copy_edit_instructions_do_not_enable_tools() {
        assert!(!tweet_rework_needs_tools(
            "footprints is not feet from an animal, find something different"
        ));
        assert!(!tweet_rework_needs_tools("replace :rocket: by :feet:"));
        assert!(tweet_rework_needs_tools("call web_search for a new cite"));
        assert!(tweet_rework_needs_tools("use browse_url on the repo"));
    }

    #[test]
    fn research_rework_includes_pack() {
        let user = tweet_rework_user_prompt(
            "TWEET-1",
            "sdk",
            "## ResearchPack\nsummary: casper sdk\n",
            "hello",
            "https://example.com/a",
            "browse_url the repo and pick a new cite",
            false,
        );
        assert!(user.contains("ResearchPack (operator asked"));
        assert!(user.contains("casper sdk"));
    }

    #[test]
    fn exploded_faq_is_rejected_short_tweet_is_not() {
        assert!(tweet_body_exploded(
            "Based on the information provided, here's a detailed and organized response:\n\n\
## What is the Interchouette Project?\n\n\
There is no publicly available information.\n\n\
### Set Up Your Rust Environment\n\n\
Make sure you have Rust installed.\n"
        ));
        assert!(!tweet_body_exploded(
            "ITCy is excited to spotlight the Casper Rust/Wasm SDK for WASM-native Layer 1 builders. 🐾✨\n\n\
https://github.com/Interchouette-ITC/casper-rust-wasm-sdk\n"
        ));
    }

    #[test]
    fn exploded_stored_body_is_not_fed_back_as_current_tweet() {
        let stored = "Tweet ID: TWEET-1\n\n\
Based on the information provided, here's a detailed response:\n\n\
## What is the Interchouette Project?\n\n\
There is no publicly available information.\n\n\
### Set Up Your Rust Environment\n\n\
Install Rust.\n\n\
Cite = option 1\n";
        let user = tweet_rework_user_prompt(
            "TWEET-1",
            "casper sdk",
            "## ResearchPack\n",
            stored,
            "https://example.com/sdk",
            "use a feet emoji, not sparkles",
            false,
        );
        assert!(user.contains("previous body was not a tweet"));
        assert!(!user.contains("What is the Interchouette Project"));
    }
}

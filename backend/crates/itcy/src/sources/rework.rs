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
    draft_rework_user_message, rework_empty_pack, tweet_rework_commentary_empty,
    tweet_rework_commentary_exploded, tweet_rework_previous_omitted, tweet_rework_user_message,
    TweetReworkUserArgs, DRAFT_REWORK_SYSTEM_CORE, TWEET_FARCE_SYSTEM_CORE,
    TWEET_REWORK_SYSTEM_CORE, WHO_IS_WHO,
};
use crate::sources::draft_footer::{
    compose_draft_message, ensure_primary_link_line, pick_link_options,
};
use crate::sources::draft_url::{extract_in_post_url, promote_link_option, set_single_in_post_url};
use crate::sources::tweet_farce::{ensure_farce_mentions, stored_is_farce};
use crate::sources::tweet_footer::{
    aerate_tweet_commentary, compose_tweet_message, ensure_operator_https_lines, ensure_option,
    ensure_tweet_cite_line, in_tweet_publisher_url, operator_https_urls, pick_tweet_cite_options,
    strip_brand_org_at_handles, tweet_body_exploded,
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

#[derive(Debug, Error)]
pub enum ReworkError {
    #[error("{0}")]
    Llm(#[from] LlmError),
    #[error("draft `{0}` is not open (status={1})")]
    NotOpen(String, String),
    #[error("rework did not produce a tweet (model dumped an essay); try shorter instructions")]
    NotATweet,
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
    let handles = crate::sources::handles::load_handles().unwrap_or_default();
    let brief_for_handles = format!("{}\n{instructions}\n{prose}", stored.subject);
    crate::sources::handles::apply_brief_handles_to_pack(&mut pack, &brief_for_handles, &handles);
    let user = draft_rework_user_message(
        instructions,
        &stored.draft_id,
        &stored.subject,
        &pack,
        &prose,
        url_lock,
    );
    let messages = vec![
        LlmMessage::system(rework_system_prompt()),
        LlmMessage::user(user),
    ];
    let (response, trace) = router
        .complete_with_tools(TaskKind::Draft, &messages, tools, 6)
        .await?;
    let mut body = crate::llm::sanitize_itcy_text(response.message.content.trim());
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
    body = strip_leading_draft_id(&body);
    body = crate::sources::handles::ensure_linkedin_brand_mention(&body);
    body = crate::sources::handles::ensure_linkedin_handle_from_pack(&body, &pack, &handles);
    body = crate::sources::draft_url::ensure_linkedin_paragraph_gaps(&body);
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

/// Fold operator rework into the stored subject so the next turn does not re-freeze
/// an outdated digest proposition as facts.
///
/// Substantive instructions **replace** the subject (they are the new truth). Short
/// polish ("add emojis") keeps the prior subject.
#[must_use]
pub fn merge_rework_subject(prior: &str, instructions: &str) -> String {
    let prior = prior.trim();
    let inst = sanitize_rework_instructions(instructions);
    if inst.is_empty() {
        return prior.to_string();
    }
    if rework_is_new_brief(&inst) {
        return inst;
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

/// Long operator notes that assert new facts are a new brief (`/tweet_about` style).
///
/// Imperative copy-edits (`remove …`, `add emojis`, `shorten`) keep the previous body
/// even when they run past the length threshold. A bare char count of 40 wrongly treated
/// `remove Comment "…" for the GitHub link.` as a full rewrite and omitted the tweet.
#[must_use]
pub fn rework_is_new_brief(instructions: &str) -> bool {
    let inst = sanitize_rework_instructions(instructions);
    if inst.is_empty() || rework_is_copy_edit(&inst) {
        return false;
    }
    inst.chars().count() >= 40
}

/// Imperative polish / surgical edit (keep previous tweet; apply the note).
#[must_use]
fn rework_is_copy_edit(instructions: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "remove ",
        "drop ",
        "delete ",
        "cut ",
        "strip ",
        "omit ",
        "take out ",
        "get rid of ",
        "no more ",
        "without ",
        "add ",
        "insert ",
        "include ",
        "put ",
        "shorten",
        "trim ",
        "tighten",
        "compress",
        "expand ",
        "lengthen",
        "fix ",
        "change ",
        "replace ",
        "swap ",
        "rewrite ",
        "rephrase ",
        "edit ",
        "update ",
        "make ",
        "don't ",
        "do not ",
        "never ",
        "keep ",
        "preserve ",
        "use ",
        "tone ",
        "less ",
        "more ",
        "punchier",
        "funnier",
        "cooler",
        "clearer",
    ];
    let t = instructions.trim_start().to_ascii_lowercase();
    PREFIXES.iter().any(|p| t.starts_with(p))
}

fn tweet_rework_needs_tools(instructions: &str) -> bool {
    let t = instructions.to_ascii_lowercase();
    t.contains("web_search")
        || t.contains("browse_url")
        || t.contains("browse ")
        || t.contains("look up")
        || t.contains("search the")
        || t.contains("new url")
        || t.contains("new cite")
        || t.contains("different url")
        || t.contains("different cite")
}

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
    let commentary = if !farce && rework_is_new_brief(instructions) {
        tweet_rework_previous_omitted().to_string()
    } else if tweet_body_exploded(&commentary_raw) {
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
) -> Result<ReworkedDraft, ReworkError> {
    if stored.status != "open" {
        return Err(ReworkError::NotOpen(
            stored.draft_id.clone(),
            stored.status.clone(),
        ));
    }
    let instructions = sanitize_rework_instructions(instructions);
    let instructions = instructions.as_str();
    let mut pack = if stored.research_pack.trim().is_empty() {
        rework_empty_pack(&stored.subject)
    } else {
        stored.research_pack.clone()
    };
    let handles = crate::sources::handles::load_handles().unwrap_or_default();
    crate::sources::handles::apply_brief_handles_to_pack(
        &mut pack,
        &format!("{}\n{instructions}", stored.subject),
        &handles,
    );
    let current = stored.link_options.first().cloned().unwrap_or_else(|| {
        crate::sources::draft_url::extract_in_post_url(&stored.body).unwrap_or_default()
    });
    let farce = stored_is_farce(&pack, &stored.body);
    info!(
        tweet_id = %stored.draft_id,
        instructions = %instructions,
        omit_previous = !farce && rework_is_new_brief(instructions),
        "rework: tweet start"
    );
    let subject_for_prompt = if !farce && rework_is_new_brief(instructions) {
        "(outdated digest one-liner omitted - OPERATOR REWORK INSTRUCTIONS above are the facts)"
    } else {
        stored.subject.as_str()
    };
    let user = tweet_rework_user_prompt(
        &stored.draft_id,
        subject_for_prompt,
        &pack,
        &stored.body,
        if current.is_empty() {
            "(none)"
        } else {
            current.as_str()
        },
        instructions,
        farce,
    );
    let (mut body, trace) = run_tweet_rework_llm(router, user, instructions, tools, farce).await?;
    if farce {
        body = ensure_farce_mentions(&body);
    }
    body = crate::sources::handles::ensure_x_handle_from_pack(&body, &pack, &handles);
    let pack_urls = stored.sources.clone();
    let (body, link_options) =
        finalize_rework_tweet_output(body, stored, &pack_urls, &current, instructions, farce);
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

async fn run_tweet_rework_llm(
    router: &FailoverRouter,
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
    let system = tweet_rework_system_prompt(farce);
    let messages = vec![LlmMessage::system(system), LlmMessage::user(user)];
    let (response, trace) = router
        .complete_with_tools(TaskKind::Draft, &messages, tools_for_call, 2)
        .await?;
    let body = scrub_rework_tweet_body(&response.message.content);
    if !tweet_body_exploded(&body) {
        return Ok((body, trace));
    }
    warn!("rework: tweet writer dumped an essay; refusing");
    Err(ReworkError::NotATweet)
}

fn scrub_rework_tweet_body(raw: &str) -> String {
    crate::llm::sanitize_itcy_text(raw.trim())
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
    fn substantive_rework_omits_previous_tweet_so_model_cannot_copy_it() {
        let user = tweet_rework_user_prompt(
            "TWEET-1",
            "mcp:// there is no mcp:// protocol right now",
            "## ResearchPack\n",
            "🚀 No mcp:// yet, but ITC is about to drop the protocol on mcpare.com.\n\nwe're building it.\n\nhttps://aaif.io/a\n",
            "https://aaif.io/a",
            "mcp:// is already being proposed as a URI scheme; ITC is only dreaming about contributing for mcpare.com",
            false,
        );
        assert!(user.contains("previous draft omitted"));
        assert!(
            !user.contains("No mcp:// yet"),
            "old claims must not be in the prompt: {user}"
        );
        assert!(
            !user.contains("we're building"),
            "old claims must not be in the prompt: {user}"
        );
        assert!(user.contains("already being proposed"));
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
        assert!(!polish.contains("previous draft omitted"));
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
            !user.contains("I manage Interchouette ITC's LinkedIn page."),
            "long correction omits previous draft so the model cannot copy it: {user}"
        );
        assert!(user.contains("previous draft omitted"));
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
    fn merge_rework_subject_replaces_outdated_digest_line() {
        let s = merge_rework_subject(
            "mcp:// there is no mcp:// protocol right now",
            "mcp:// is already proposed as a URI scheme for MCP discovery; ITC is only dreaming about contributing for mcpare.com",
        );
        assert!(s.contains("already proposed"));
        assert!(s.contains("contributing"));
        assert!(
            !s.contains("there is no mcp://"),
            "must not keep the wrong digest line: {s}"
        );
        let short = merge_rework_subject("rust policy", "add emojis");
        assert!(short.contains("rust policy"));
        assert!(short.contains("[operator rework]"));
    }

    #[test]
    fn surgical_remove_line_keeps_previous_tweet_not_new_brief() {
        let instructions = "remove Comment \"obscura\" for the GitHub link.";
        assert!(
            instructions.chars().count() >= 40,
            "regression needs a long-enough remove note"
        );
        assert!(
            !rework_is_new_brief(instructions),
            "remove-line edits must not omit the previous tweet"
        );
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
        assert!(
            !user.contains("previous draft omitted"),
            "surgical remove must keep previous body: {user}"
        );
        assert!(
            user.contains("Obscura"),
            "previous commentary must stay in the prompt: {user}"
        );
        let merged = merge_rework_subject(
            "Obscura Rust browser | cite https://x.com/thevibefounder/status/1",
            instructions,
        );
        assert!(
            merged.contains("Obscura Rust browser"),
            "subject must not be replaced by the remove note alone: {merged}"
        );
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
        assert!(tweet_rework_needs_tools(
            "browse the repo and pick a new cite"
        ));
        assert!(tweet_rework_needs_tools("use a different url"));
    }

    #[test]
    fn research_rework_includes_pack() {
        let user = tweet_rework_user_prompt(
            "TWEET-1",
            "sdk",
            "## ResearchPack\nsummary: casper sdk\n",
            "hello",
            "https://example.com/a",
            "browse the repo and pick a new cite",
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

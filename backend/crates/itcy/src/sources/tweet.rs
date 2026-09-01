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
    resolve_session_draft_id, run_load_phase, GroundedDraft, RagError,
};
use crate::sources::tweet_footer::{
    aerate_tweet_commentary, coerce_tweet_body, compose_tweet_message, ensure_tweet_cite_line,
    extract_brief_cite, in_tweet_publisher_url, operator_https_urls, pick_tweet_cite_options,
    strip_brand_org_at_handles, tweet_body_exploded,
};
use crate::sources::tweet_load::run_short_cite_load;
use crate::sources::url_hygiene::is_x_status_url;
use crate::tools::ItcyTools;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

const TWEET_SCRUB_RETRY_NOTE: &str =
    "HARD REWRITE: Write only from ResearchPack. Name entities and numbers from the cite. \
Do not paste the brief opening. Forbidden: not-just / not-about contrast lines \
(\"It's not just X, it's Y\"). 3-4 aerated beats with 2-3 unique emoji.";

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
        run_short_cite_load(subject, url, tools, false).await?
    } else {
        run_load_phase(router, subject, "", tools, tools_dyn, session_dir.as_ref()).await?
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
    // Capture before end_session clears EXTRACTED (same timing bug as draft Link refill).
    let refill_pool = crate::sources::rag::draft_link_refill_pool(tools, &pack_urls).await;
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
    let mut pack_for_links = pack_urls.clone();
    for u in refill_pool
        .iter()
        .chain(operator_https_urls(subject).iter())
    {
        if pack_for_links.len() >= crate::sources::publisher_url::LINK_OPTIONS_CAP {
            break;
        }
        if !pack_for_links.iter().any(|x| x == u) {
            pack_for_links.push(u.clone());
        }
    }
    let (body, link_options) = attach_tweet_cites(&tweet_body, &pack_for_links, &tweet_id, subject);
    crate::sources::publisher_url::require_tweet_link_options_floor(subject, &link_options)
        .map_err(RagError::Store)?;
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
    let (body, link_options) = attach_tweet_cites(&tweet_body, &urls, &tweet_id, subject);
    crate::sources::publisher_url::require_tweet_link_options_floor(subject, &link_options)
        .map_err(RagError::Store)?;
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

struct TweetWriterCtx<'a> {
    router: &'a FailoverRouter,
    subject: &'a str,
    research_pack: &'a str,
    pack_urls: &'a [String],
    subject_https: bool,
    session_dir: Option<&'a PathBuf>,
    tools: Option<&'a ItcyTools>,
    extra_pack_note: Option<&'a str>,
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
    let base = TweetWriterCtx {
        router,
        subject,
        research_pack,
        pack_urls,
        subject_https,
        session_dir,
        tools,
        extra_pack_note: None,
    };
    let (content, trace) = complete_tweet_writer(&base).await?;
    match finalize_tweet_writer_body(&content, subject) {
        Ok(body) => Ok((body, trace)),
        Err(first_err) => {
            warn!(error = %first_err, "load_tweet: scrub failed; retrying writer once");
            let retry_ctx = TweetWriterCtx {
                extra_pack_note: Some(TWEET_SCRUB_RETRY_NOTE),
                ..base
            };
            let (retry_content, retry_trace) = complete_tweet_writer(&retry_ctx).await?;
            let body = finalize_tweet_writer_body(&retry_content, subject)?;
            Ok((body, trace.accumulate(&retry_trace)))
        }
    }
}

async fn complete_tweet_writer(
    ctx: &TweetWriterCtx<'_>,
) -> Result<(String, CompletionTrace), RagError> {
    let mut pack_note = tweet_pack_note(ctx.pack_urls.is_empty(), ctx.subject_https).to_string();
    if let Some(note) = ctx.extra_pack_note {
        pack_note.push_str("\n\n");
        pack_note.push_str(note);
    }
    let user = tweet_user_message(ctx.research_pack, &pack_note, ctx.subject);
    let messages = vec![
        LlmMessage::system(tweet_system_prompt()),
        LlmMessage::user(user),
    ];
    match ctx
        .router
        .complete_with_tools(TaskKind::Draft, &messages, None, 0)
        .await
    {
        Ok((response, trace)) => Ok((response.message.content, trace)),
        Err(e) => {
            end_session_best_effort(
                ctx.tools,
                ctx.session_dir,
                &format!("tweet writer failed: {e}"),
            )
            .await;
            Err(e.into())
        }
    }
}

fn finalize_tweet_writer_body(content: &str, subject: &str) -> Result<String, RagError> {
    let body = scrub_and_validate_tweet_body(content)?;
    if tweet_body_exploded(&body) {
        warn!("load_tweet: writer dump coerced to tweet shape (no retry)");
        return Ok(coerce_tweet_body(&body, subject));
    }
    Ok(body)
}

fn tweet_has_commentary_beats(body: &str) -> bool {
    body.split("\n\n").any(|para| {
        let t = para.trim();
        if t.is_empty() || t.starts_with("https://") {
            return false;
        }
        !t.split_whitespace()
            .all(|tok| tok.starts_with('#') && tok.len() > 1)
    })
}

pub(crate) fn attach_tweet_cites(
    body: &str,
    pack_urls: &[String],
    tweet_id: &str,
    brief: &str,
) -> (String, Vec<String>) {
    let prefer = extract_brief_cite(brief);
    let operator_urls = operator_https_urls(brief);
    let body = crate::sources::draft_url::strip_sources_section(&aerate_tweet_commentary(body));
    let body = crate::sources::tweet_footer::strip_own_x_handle(&body);
    let has_non_x_operator = operator_urls.iter().any(|u| !is_x_status_url(u));
    let body = if has_non_x_operator {
        strip_brand_org_at_handles(&body)
    } else {
        body
    };
    let mut link_options = pick_tweet_cite_options(
        &crate::sources::corpus_propose::filter_pack_urls_for_subject(pack_urls, brief),
        &body,
    );
    for u in &operator_urls {
        crate::sources::tweet_footer::ensure_option(&mut link_options, u);
    }
    if let Some(cite) = prefer.as_deref() {
        crate::sources::draft_url::promote_link_option(&mut link_options, cite);
        // One in-post cite only. Extra pack / brief URLs stay in Link options, not the body.
        let body = ensure_tweet_cite_line(&body, Some(cite));
        return (
            compose_tweet_message(&body, tweet_id, &link_options),
            link_options,
        );
    }
    let primary = in_tweet_publisher_url(&link_options).map(str::to_string);
    let body = ensure_tweet_cite_line(&body, primary.as_deref());
    (
        compose_tweet_message(&body, tweet_id, &link_options),
        link_options,
    )
}

/// Sanitize tweet writer output and drop banned slogan-mush beats (salvage-only).
#[must_use]
pub(crate) fn scrub_tweet_body(raw: &str) -> String {
    let raw = crate::llm::sanitize_itcy_text(raw);
    let raw = crate::sources::draft_url::strip_sources_section(&raw);
    let raw = crate::sources::tweet_footer::strip_own_x_handle(&raw);
    if crate::sources::corpus_propose::body_has_slogan_mush(&raw) {
        let stripped = crate::sources::corpus_propose::strip_slogan_mush_sentences(&raw);
        if !stripped.trim().is_empty() {
            warn!("load_tweet: slogan mush stripped from writer body");
            return stripped;
        }
    }
    raw
}

/// Writer path: sanitize, salvage mush, hard-fail when mush or empty body remains.
pub(crate) fn scrub_and_validate_tweet_body(raw: &str) -> Result<String, RagError> {
    let body = scrub_tweet_body(raw);
    if body.trim().is_empty() {
        return Err(RagError::Store(
            "writer returned empty tweet after scrub".into(),
        ));
    }
    if crate::sources::corpus_propose::body_has_slogan_mush(&body) {
        return Err(RagError::Store(
            "writer kept banned X slogan mush after salvage".into(),
        ));
    }
    if !tweet_has_commentary_beats(&body) {
        return Err(RagError::Store(
            "writer left no commentary beats after scrub".into(),
        ));
    }
    Ok(body)
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
    fn scrub_tweet_body_strips_not_just_mush_keeps_other_beats() {
        let raw = "\
🚀 @DoorDash shifted 130k tasks to Flux, no more laptop clutter.

🤖 Firecracker microVMs + MCP gateways? Scoped access beats laptop sprawl.

🦉 25k code reviews weekly. It's not just automation, it's control.

#DevOps #CloudNative #AIInfra";
        let out = scrub_tweet_body(raw);
        assert!(
            out.contains("130k tasks"),
            "concrete hook beat must survive: {out}"
        );
        assert!(
            out.contains("Firecracker"),
            "angle beat must survive: {out}"
        );
        assert!(
            !out.contains("not just automation"),
            "mush beat must drop: {out}"
        );
        assert!(out.contains("#DevOps"), "hashtag line must survive: {out}");
    }

    #[test]
    fn scrub_and_validate_tweet_body_fails_when_only_mush_remains() {
        let raw = "It's not just automation, it's control.\n\n#DevOps";
        let err = scrub_and_validate_tweet_body(raw).unwrap_err();
        assert!(
            err.to_string().contains("commentary beats"),
            "expected empty-commentary failure: {err}"
        );
    }

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
        let brief = format!("tinyboot, cite {prefer}");
        let (out, opts) = attach_tweet_cites(raw, &pack, "TWEET-1", &brief);
        assert!(!out.contains("Sources:"));
        assert_eq!(opts[0], prefer);
        assert!(
            opts.iter().any(|u| u.contains("neowin.net")),
            "publisher from pack must stay in Link options: {opts:?}"
        );
        assert!(opts.len() >= 2, "X cite + publisher: {opts:?}");
        let api = crate::publish::tweet_text_for_api(&out);
        assert!(api.contains(prefer));
        assert!(
            !api.contains("neowin"),
            "publisher not in tweet body: {api}"
        );
        assert!(api.contains("tinyboot"));
    }

    #[test]
    fn locked_x_status_cite_passes_link_floor_with_one_option() {
        let x = "https://x.com/nineshoot/status/2094567713113059575";
        let brief = format!("Obscura Rust browser, cite {x}");
        let pack = vec![x.to_string()];
        let (_out, opts) = attach_tweet_cites(
            "🚀 Obscura ships Rust + V8 + CDP.\n\n#Rust #Browser\n",
            &pack,
            "TWEET-098",
            &brief,
        );
        crate::sources::publisher_url::require_tweet_link_options_floor(&brief, &opts)
            .expect("locked X cite with one Link option");
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0], x);
    }

    #[test]
    fn x_status_cite_keeps_at_least_three_link_options_from_pack() {
        let x = "https://x.com/a/status/1";
        let p1 = "https://labs.sogeti.com/article-one";
        let p2 = "https://decrypt.co/999/some-post";
        let brief = format!("topic, cite {x}");
        let pack = vec![x.into(), p1.into(), p2.into()];
        let (_out, opts) =
            attach_tweet_cites("One line beat.\n\n#Rust\n", &pack, "TWEET-3", &brief);
        assert!(
            opts.len() >= crate::sources::publisher_url::LINK_OPTIONS_MIN,
            "tweet Link options must meet floor of 3: {opts:?}"
        );
        assert_eq!(opts[0], x);
        assert!(opts.iter().any(|u| u == p1));
        assert!(opts.iter().any(|u| u == p2));
    }

    #[test]
    fn attach_tweet_cites_one_body_cite_extras_in_link_options() {
        // New contract (same as LinkedIn draft): one https in body; extras in Link options.
        let raw = "\
Magecart still lives.

Evaluator from @Interchouette-ITC helps.

#Security

https://x.com/arnaudmerigeau/status/2090774291897786849
";
        let x = "https://x.com/arnaudmerigeau/status/2090774291897786849";
        let gh = "https://github.com/Interchouette-ITC/evaluator";
        let brief = format!("Magecart, cite {x} and promote Evaluator at {gh}");
        let pack = vec![x.into(), gh.into()];
        let (out, opts) = attach_tweet_cites(raw, &pack, "TWEET-2", &brief);
        let api = crate::publish::tweet_text_for_api(&out);
        assert!(api.contains(x), "primary cite in body: {api}");
        assert!(
            !api.contains(gh),
            "extra brief URL must not spam the tweet body: {api}"
        );
        assert!(!api.contains("@Interchouette-ITC"));
        assert!(opts.iter().any(|u| u == gh), "extra URL in Link options");
        assert_eq!(opts[0], x);
        assert_eq!(
            api.lines()
                .filter(|l| l.trim().starts_with("https://"))
                .count(),
            1,
            "one in-post cite only: {api}"
        );
    }

    #[test]
    fn casper_digest_brief_three_urls_one_in_tweet_body() {
        // Regression TWEET-085: payouts + status + thepaypers all landed in the body.
        let payouts = "https://payouts.com";
        let status = "https://x.com/Casper_Network/status/2092237696139698529";
        let article = "https://thepaypers.com/payments/news/payoutscom-casper-association-partner-on-ai-agent-payments";
        let brief = format!("Payouts.com has picked Casper.\n\n{payouts}\n\n{status}\n\n{article}");
        let raw = format!(
            "Payouts.com is letting AI agents spend without human keys.\n\
Casper settles every tiny payment, no extra cost.\n\
Thousands of transactions, zero friction.\n\n\
#AI #Payments #Casper\n\n\
{article}\n"
        );
        let pack = vec![
            article.into(),
            status.into(),
            "https://finance.yahoo.com/technology/ai/articles/ode-anthropic-acquires-casper-studios-150000200.html"
                .into(),
        ];
        let (out, opts) = attach_tweet_cites(&raw, &pack, "TWEET-085", &brief);
        let api = crate::publish::tweet_text_for_api(&out);
        assert_eq!(
            api.lines()
                .filter(|l| l.trim().starts_with("https://"))
                .count(),
            1,
            "one cite in body like LinkedIn draft: {api}"
        );
        assert!(api.contains(payouts), "Link:1 from first brief cite: {api}");
        assert!(opts.iter().any(|u| u == article));
        assert!(opts.iter().any(|u| u == status));
        assert!(out.contains("Link: 1"));
    }
}

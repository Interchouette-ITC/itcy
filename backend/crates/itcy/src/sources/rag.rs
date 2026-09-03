// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Load (research pack) → Draft (writer) pipeline with shared tools.

use crate::llm::agent::ToolProvider;
use crate::llm::client::{CompletionTrace, LlmMessage, LlmResponse};
use crate::llm::clock::today_context_line;
use crate::llm::disclosure::with_disclosure;
use crate::llm::router::{FailoverRouter, TaskKind};
use crate::llm::LlmError;
use crate::sources::embed::{default_embed_model, EmbedClient};
use crate::sources::store::{cosine_similarity, ChunkRecord, SourceDb};
use crate::tools::ItcyTools;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::{info, warn};

/// Visual pipeline break in the product log pane (blank line + banner in tracing stream).
pub(crate) fn log_pipeline_banner(label: &str) {
    let banner = format!("──────── {label} ────────");
    eprintln!();
    eprintln!("{banner}");
    info!("{banner}");
    eprintln!();
}

/// Sub-step inside a LOAD/DRAFT banner (blank line + short rule).
pub(crate) fn log_pipeline_step(step: &str) {
    eprintln!();
    let line = format!("── {step} ──");
    eprintln!("{line}");
    info!("{line}");
}

pub use crate::prompts::{
    AI_CMO, CREATIVE_LINKEDIN, DRAFT_SYSTEM, DRAFT_SYSTEM_CORE, FORM_CRAFT_LINKEDIN,
    LOAD_SYSTEM_CORE, WHO_IS_WHO,
};

use crate::prompts::{draft_pack_note, draft_user_message, load_user_message};

/// Full load system prompt including today's date.
#[must_use]
pub fn load_system_prompt() -> String {
    format!(
        "{}\n\n{}\n\n{}",
        today_context_line(),
        WHO_IS_WHO,
        LOAD_SYSTEM_CORE
    )
}

/// Full draft system prompt including today's date.
#[must_use]
pub fn draft_system_prompt() -> String {
    format!(
        "{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}",
        today_context_line(),
        WHO_IS_WHO,
        AI_CMO,
        CREATIVE_LINKEDIN,
        FORM_CRAFT_LINKEDIN,
        DRAFT_SYSTEM_CORE
    )
}

/// Max tool-loop *iterations* per LOAD or DRAFT pass.
/// One iteration = model replies once and may call tools; not "3 searches".
/// Typical LOAD: 1 `web_search` + 1-2 `browse_url`. Draft: corpus + browse + write.
/// Code also refuses a 2nd search before browse. Final capped turn runs without tools.
pub(crate) const MAX_TOOL_ROUNDS: u32 = 4;

/// Errors building a grounded draft.
#[derive(Debug, Error)]
pub enum RagError {
    #[error("rag store: {0}")]
    Store(String),
    #[error("rag embed: {0}")]
    Embed(String),
    #[error("rag llm: {0}")]
    Llm(#[from] LlmError),
    #[error("no grounded sources for subject `{0}`")]
    NoSources(String),
    #[error("writer dumped an essay instead of a tweet")]
    NotATweet,
    #[error("farce tweet missing @grok @cursor_ai @elonmusk")]
    FarceMissingMentions,
}

/// One retrieved snippet for prompting / citation.
#[derive(Debug, Clone)]
pub struct RetrievedChunk {
    pub subject: String,
    pub text: String,
    pub score: f32,
}

/// Retrieves top-k chunks for a subject query via filter + cosine similarity.
///
/// # Errors
///
/// Returns a [`RagError`] variant for load/draft/LLM/store failure.
pub async fn retrieve_for_subject(
    db_path: &Path,
    embed: &dyn EmbedClient,
    subject: &str,
    top_k: usize,
) -> Result<Vec<RetrievedChunk>, RagError> {
    let model = default_embed_model();
    let query_vec = embed
        .embed(&model, subject)
        .await
        .map_err(|e| RagError::Embed(e.to_string()))?;
    score_candidates(db_path, subject, &query_vec, top_k)
}

fn score_candidates(
    db_path: &Path,
    subject: &str,
    query_vec: &[f32],
    top_k: usize,
) -> Result<Vec<RetrievedChunk>, RagError> {
    let db = SourceDb::open(db_path).map_err(|e| RagError::Store(e.to_string()))?;
    let candidates = db
        .get_chunk_candidates(subject, 200)
        .map_err(|e| RagError::Store(e.to_string()))?;
    let mut scored: Vec<(f32, ChunkRecord)> = candidates
        .into_iter()
        .map(|c| {
            let score = cosine_similarity(query_vec, &c.embedding);
            (score, c)
        })
        .collect();
    if scored.is_empty() {
        let all = db
            .get_chunk_candidates("", 200)
            .map_err(|e| RagError::Store(e.to_string()))?;
        scored = all
            .into_iter()
            .map(|c| {
                let score = cosine_similarity(query_vec, &c.embedding);
                (score, c)
            })
            .collect();
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let out: Vec<RetrievedChunk> = scored
        .into_iter()
        .take(top_k)
        .filter(|(score, _)| *score > 0.05)
        .map(|(score, c)| RetrievedChunk {
            subject: c.subject,
            text: c.text,
            score,
        })
        .collect();
    if out.is_empty() {
        return Err(RagError::NoSources(subject.to_string()));
    }
    Ok(out)
}

/// Grounded draft plus disclosure metadata for BAT submit.
#[derive(Debug, Clone)]
pub struct GroundedDraft {
    pub subject: String,
    pub body: String,
    /// Stable operator reference (`DRAFT-YYYYMMDD-NNNNNN`), also first line of `body`.
    pub draft_id: String,
    pub model: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub source_labels: Vec<String>,
    pub link_options: Vec<String>,
    pub research_pack: String,
}

fn allocate_draft_id_fallback(db_path: &Path) -> String {
    crate::sources::draft_footer::next_draft_id(db_path).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "load_draft: draft id allocate failed; using fallback");
        format!("DRAFT-{}-UNKNOWN", chrono::Local::now().format("%Y%m%d"))
    })
}

/// Start session (and product.log tee) before LOAD banner so the full phase is captured.
pub(crate) async fn begin_load_session_dir(
    tools: Option<&ItcyTools>,
    db_path: &Path,
    subject: &str,
) -> Option<PathBuf> {
    let t = tools?;
    let draft_id = t
        .session_draft_id()
        .await
        .unwrap_or_else(|| allocate_draft_id_fallback(db_path));
    match t.begin_research_session(subject, &draft_id).await {
        Ok(d) => {
            info!(
                dir = %d.display(),
                draft_id = %draft_id,
                "load_draft: research session"
            );
            Some(d)
        }
        Err(e) => {
            tracing::warn!(error = %e, "load_draft: session start failed; continuing");
            None
        }
    }
}

fn empty_research_pack(subject: &str) -> String {
    format!(
        "## ResearchPack\n\
subject: {subject}\n\
ai_overview: \n\
summary: LOAD returned empty text (model or tools failed). Do NOT invent a CEO story, quotes, or URLs.\n\
candidates:\n\
rejected:\n\
notes: empty load response\n"
    )
}

fn merge_urls_cap(
    pack_urls: &mut Vec<String>,
    extras: impl IntoIterator<Item = String>,
    cap: usize,
) {
    for u in extras {
        if !pack_urls.iter().any(|x| x == &u) && pack_urls.len() < cap {
            pack_urls.push(u);
        }
    }
}

/// Recover publisher URLs from session browses into the `ResearchPack`.
///
/// SERP EXTRACTED links fill the pack only when LOAD listed none and never browsed.
/// Dumping the whole SERP into Link options mixes homonyms (Rust Cargo vs logistics cargo).
async fn enrich_research_pack_urls(
    tools: Option<&ItcyTools>,
    research_pack: &mut String,
) -> Vec<String> {
    let browsed_urls = if let Some(t) = tools {
        filter_publisher_urls(&t.session_browsed_urls().await)
    } else {
        Vec::new()
    };
    let extracted_urls = if let Some(t) = tools {
        filter_publisher_urls(&t.session_extracted_urls().await)
    } else {
        Vec::new()
    };
    let from_pack = filter_publisher_urls(&extract_http_urls(research_pack));
    if from_pack.is_empty() && !browsed_urls.is_empty() {
        research_pack.push_str(
            "\n\n## RecoveredCandidates (session browses; model omitted URLs from pack)\n",
        );
        for u in &browsed_urls {
            let _ = writeln!(
                research_pack,
                "- final_url={u} | title=(from browse) | why=session-browse | browsed=yes"
            );
        }
        info!(
            recovered = %browsed_urls.join(" | "),
            "load_draft: recovered pack URLs from session browses"
        );
    }
    let mut pack_urls = pack_urls_from_load(&from_pack, &browsed_urls, &extracted_urls);
    // If LOAD listed candidates but never browsed, open the best publisher URL once.
    if let Some(t) = tools {
        let already = t.session_browsed_urls().await;
        if already.is_empty() {
            if let Some(u) = pack_urls.first().cloned() {
                info!(url = %u, "load_draft: auto-browse top pack URL (model skipped browse)");
                match t.research_browse(&u).await {
                    Ok(_) => {
                        if pack_urls.iter().any(|x| x == &u) {
                            pack_urls.retain(|x| x != &u);
                        }
                        pack_urls.insert(0, u);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, url = %u, "load_draft: auto-browse failed");
                        pack_urls.retain(|x| x != &u);
                    }
                }
            }
        }
    }
    if let Some(t) = tools {
        for (url, text) in t.session_browse_excerpts().await {
            append_browsed_page_to_pack(research_pack, &url, &text);
        }
    }
    crate::sources::publisher_url::filter_reachable_publisher_urls(pack_urls).await
}

/// Pack cites: deterministic SERP EXTRACTED first (MERGED order), then browsed, then LLM text.
fn pack_urls_from_load(
    from_pack: &[String],
    browsed: &[String],
    extracted: &[String],
) -> Vec<String> {
    use crate::sources::publisher_url::LINK_OPTIONS_CAP;
    let mut pack_urls: Vec<String> = Vec::new();
    merge_urls_cap(
        &mut pack_urls,
        filter_publisher_urls(extracted),
        LINK_OPTIONS_CAP,
    );
    merge_urls_cap(&mut pack_urls, browsed.iter().cloned(), LINK_OPTIONS_CAP);
    merge_urls_cap(
        &mut pack_urls,
        filter_publisher_urls(from_pack),
        LINK_OPTIONS_CAP,
    );
    pack_urls
}

fn append_browsed_page_to_pack(pack: &mut String, url: &str, tool_out: &str) {
    let url = url.trim();
    let excerpt = tool_out.trim();
    if url.is_empty() || excerpt.is_empty() {
        return;
    }
    let marker = format!("url: {url}");
    if pack.contains(&marker) && pack.contains("## Browsed page") {
        return;
    }
    pack.push_str("\n\n## Browsed page (this is the cite; do not switch industry or topic)\n");
    pack.push_str(&marker);
    pack.push('\n');
    pack.push_str(excerpt);
    pack.push('\n');
}

fn format_serp_seed_block(query: &str, extracted: &[String]) -> String {
    let mut block = format!(
        "## SERP (deterministic)\nquery: {query}\nUse these MERGED EXTRACTED links (SERP order). browse_url 1-2 of them.\n"
    );
    for u in extracted
        .iter()
        .take(crate::sources::publisher_url::LINK_OPTIONS_CAP)
    {
        let _ = writeln!(
            block,
            "- final_url={u} | title=(from SERP) | why=serp-merged | browsed=no"
        );
    }
    block
}

async fn run_deterministic_serp_search(
    tools: Option<&ItcyTools>,
    subject: &str,
    instructions: &str,
) -> String {
    let Some(t) = tools else {
        return String::new();
    };
    let query = crate::sources::tweet_footer::web_search_query(subject, instructions);
    log_pipeline_step("LOAD serp");
    info!(query = %query, "load_draft: deterministic web_search");
    match t.research_web_search(&query).await {
        Ok(_) => {
            let extracted = filter_publisher_urls(&t.session_extracted_urls().await);
            if extracted.is_empty() {
                warn!(query = %query, "load_draft: deterministic SERP returned no EXTRACTED links");
                String::new()
            } else {
                info!(
                    n = extracted.len(),
                    urls = %extracted.join(" | "),
                    "load_draft: deterministic SERP EXTRACTED"
                );
                t.set_load_serp_policy().await;
                format_serp_seed_block(&query, &extracted)
            }
        }
        Err(e) => {
            warn!(error = %e, query = %query, "load_draft: deterministic web_search failed");
            String::new()
        }
    }
}

pub(crate) async fn run_load_phase(
    router: &FailoverRouter,
    subject: &str,
    instructions: &str,
    tools: Option<&ItcyTools>,
    tools_dyn: Option<&dyn ToolProvider>,
    session_dir: Option<&PathBuf>,
) -> Result<(String, Vec<String>, CompletionTrace), RagError> {
    let operator_brief = crate::sources::tweet_footer::operator_brief(subject, instructions);
    log_pipeline_banner("LOAD (research pack)");
    let serp_seed = run_deterministic_serp_search(tools, subject, instructions).await;
    log_pipeline_step("LOAD llm");
    info!(subject = %operator_brief, "load_draft: start");
    info!("load_draft: calling LLM (tool loop; may take a while with no further logs)");

    let user_body = load_user_message(&operator_brief);
    let user = if serp_seed.is_empty() {
        user_body
    } else {
        format!("{serp_seed}\n\n{user_body}")
    };
    let load_messages = vec![
        LlmMessage::system(load_system_prompt()),
        LlmMessage::user(user),
    ];
    let (load_response, load_trace) = match router
        .complete_with_tools(TaskKind::Load, &load_messages, tools_dyn, MAX_TOOL_ROUNDS)
        .await
    {
        Ok(v) => {
            info!(
                provider = %v.1.provider,
                model = %v.1.model,
                prompt_tokens = v.1.prompt_tokens,
                completion_tokens = v.1.completion_tokens,
                "load_draft: LLM tool loop returned"
            );
            v
        }
        Err(e) => {
            end_session_best_effort(tools, session_dir, &format!("load failed: {e}")).await;
            return Err(e.into());
        }
    };
    let mut research_pack = {
        let raw = load_response.message.content.trim().to_string();
        if raw.is_empty() {
            if serp_seed.is_empty() {
                empty_research_pack(&operator_brief)
            } else {
                format!("{serp_seed}\nnotes: LLM returned empty pack; SERP seed kept\n")
            }
        } else if serp_seed.is_empty() {
            raw
        } else {
            format!("{serp_seed}\n\n{raw}")
        }
    };
    let pack_urls = enrich_research_pack_urls(tools, &mut research_pack).await;
    log_pipeline_step("LOAD pack");
    info!(
        load_model = %load_trace.model_label(),
        pack_chars = research_pack.len(),
        pack_urls = %if pack_urls.is_empty() {
            "(none)".to_string()
        } else {
            pack_urls.join(" | ")
        },
        "load_draft: ResearchPack ready"
    );
    Ok((research_pack, pack_urls, load_trace))
}

struct DraftPhaseCtx<'a> {
    router: &'a FailoverRouter,
    subject: &'a str,
    research_pack: &'a str,
    pack_urls: &'a [String],
    tools: Option<&'a ItcyTools>,
    brief_has_cite: bool,
    session_dir: Option<&'a PathBuf>,
    extra_pack_note: Option<&'a str>,
}

const DRAFT_SCRUB_RETRY_NOTE: &str =
    "HARD REWRITE: Write only from ResearchPack. Name entities and numbers from the cite page. \
Do not paste the brief opening. Never start with the word cite (that is instruction vocabulary). \
No planning monologue. Dense LinkedIn paragraphs with emoji.";

async fn run_draft_phase(
    ctx: &DraftPhaseCtx<'_>,
) -> Result<(LlmResponse, CompletionTrace), RagError> {
    if let Some(t) = ctx.tools {
        if ctx.brief_has_cite {
            t.set_draft_subject_https_writer_policy().await;
        } else {
            t.set_draft_policy(ctx.pack_urls).await;
        }
    }

    let tools_dyn: Option<&dyn ToolProvider> = if ctx.brief_has_cite {
        None
    } else {
        ctx.tools.map(|t| t as &dyn ToolProvider)
    };

    log_pipeline_banner("DRAFT (writer)");

    let base_note = draft_pack_note(ctx.pack_urls.is_empty(), ctx.brief_has_cite);
    let mut pack_note = match ctx.extra_pack_note {
        Some(extra) if !extra.trim().is_empty() => format!("{base_note}\n\n{extra}"),
        _ => base_note.to_string(),
    };
    if let Some(qnote) = crate::sources::draft_footer::operator_quote_pack_note(ctx.subject) {
        pack_note.push_str("\n\n");
        pack_note.push_str(&qnote);
    }
    let user = if ctx.brief_has_cite {
        crate::prompts::draft_user_message_subject_https(ctx.research_pack, &pack_note, ctx.subject)
    } else {
        draft_user_message(ctx.research_pack, &pack_note, ctx.subject)
    };
    let draft_messages = vec![
        LlmMessage::system(draft_system_prompt()),
        LlmMessage::user(user),
    ];
    match ctx
        .router
        .complete_with_tools(TaskKind::Draft, &draft_messages, tools_dyn, MAX_TOOL_ROUNDS)
        .await
    {
        Ok(v) => Ok(v),
        Err(e) => {
            end_session_best_effort(ctx.tools, ctx.session_dir, &format!("draft failed: {e}"))
                .await;
            Err(e.into())
        }
    }
}

pub(crate) async fn resolve_session_draft_id(tools: Option<&ItcyTools>, db_path: &Path) -> String {
    if let Some(t) = tools {
        if let Some(id) = t.session_draft_id().await {
            return id;
        }
    }
    allocate_draft_id_fallback(db_path)
}

/// Inject registry handles from the operator brief (name / URL / `@`), then brand if named.
pub(crate) fn apply_pack_handles(tools: Option<&ItcyTools>, brief: &str, pack: &mut String) {
    if let Some(t) = tools {
        let idx = t.handles_index();
        crate::sources::handles::apply_brief_handles_to_pack(pack, brief, &idx);
        return;
    }
    let owned = crate::sources::handles::load_handles().unwrap_or_default();
    crate::sources::handles::apply_brief_handles_to_pack(pack, brief, &owned);
}

fn ensure_body_handles_from_pack(tools: Option<&ItcyTools>, body: &str, pack: &str) -> String {
    if let Some(t) = tools {
        let idx = t.handles_index();
        return crate::sources::handles::ensure_linkedin_handle_from_pack(body, pack, &idx);
    }
    let owned = crate::sources::handles::load_handles().unwrap_or_default();
    crate::sources::handles::ensure_linkedin_handle_from_pack(body, pack, &owned)
}

/// Persist research pack while status stays `building` (survives restart mid-writer).
pub(crate) async fn checkpoint_building_pack(
    db_path: &Path,
    tools: Option<&ItcyTools>,
    subject: &str,
    research_pack: &str,
    pack_urls: &[String],
) {
    let draft_id = resolve_session_draft_id(tools, db_path).await;
    let Ok(store) = crate::bat::store::DraftStore::open(db_path) else {
        return;
    };
    let Ok(Some(mut row)) = store.get(&draft_id) else {
        let mut stub = crate::bat::store::stored_building_stub(&draft_id, subject.trim());
        stub.research_pack = research_pack.to_string();
        stub.sources = pack_urls.to_vec();
        let _ = store.upsert(&stub);
        return;
    };
    if row.status != crate::bat::store::status::BUILDING
        && row.status != crate::bat::store::status::OPEN
    {
        return;
    }
    row.research_pack = research_pack.to_string();
    if !pack_urls.is_empty() {
        row.sources = pack_urls.to_vec();
    }
    row.updated_at = String::new();
    let _ = store.upsert(&row);
}

pub(crate) fn scrub_and_validate_writer_body(
    body_raw: &str,
    pack_urls: &[String],
    paste_subject: &str,
) -> Result<String, RagError> {
    info!(
        draft_chars = body_raw.trim().len(),
        "load_draft: raw writer chars"
    );
    if body_raw.trim().is_empty() {
        return Err(RagError::Store(
            "writer returned empty content (no LinkedIn post)".into(),
        ));
    }
    let mut body = scrub_invented_urls(&crate::llm::sanitize_itcy_text(body_raw));
    body = scrub_urls_outside_pack(&body, pack_urls);
    body = crate::sources::draft_url::strip_sources_section(&body);
    body = crate::sources::draft_footer::strip_leading_page_title_lede(&body);
    body = crate::sources::draft_footer::strip_leading_cite_instruction(&body);
    if looks_like_writer_scratchpad(&body) {
        warn!(
            prose_words = prose_word_count(&body),
            "load_draft: writer returned planning/monologue; refusing to post"
        );
        return Err(RagError::Store(
            "writer returned planning monologue instead of a LinkedIn post".into(),
        ));
    }
    if body_copies_operator_subject(&body, paste_subject) {
        let stripped = strip_leading_subject_paste(&body, paste_subject);
        if stripped.trim().is_empty() || body_copies_operator_subject(&stripped, paste_subject) {
            return Err(RagError::Store(
                "writer pasted the subject without adding commentary".into(),
            ));
        }
        warn!("load_draft: stripped leading subject paste from writer body");
        body = stripped;
    }
    if looks_like_x_shaped_linkedin(&body) {
        return Err(RagError::Store(
            "writer returned X-shaped LinkedIn (blank-line beats or hashtag lines)".into(),
        ));
    }
    body = ensure_draft_emoji_bar(&body);
    if crate::sources::corpus_propose::body_has_slogan_mush(&body) {
        let stripped = crate::sources::corpus_propose::strip_slogan_mush_sentences(&body);
        if stripped.trim().is_empty()
            || crate::sources::corpus_propose::body_has_slogan_mush(&stripped)
        {
            return Err(RagError::Store(
                "writer kept banned LinkedIn slogan mush after salvage".into(),
            ));
        }
        warn!("load_draft: slogan mush stripped from writer body");
        body = ensure_draft_emoji_bar(&stripped);
    }
    body = strip_spurious_period_after_emoji(&body);
    body = crate::sources::draft_footer::aerate_linkedin_draft(&body);
    Ok(body)
}

async fn draft_body_with_scrub_retry(
    ctx: &DraftPhaseCtx<'_>,
    paste_subject: &str,
) -> Result<(String, CompletionTrace), RagError> {
    let (draft_response, mut draft_trace) = run_draft_phase(ctx).await?;
    let mut body = match scrub_and_validate_writer_body(
        &draft_response.message.content,
        ctx.pack_urls,
        paste_subject,
    ) {
        Ok(body) => body,
        Err(first_err) => {
            warn!(error = %first_err, "load_draft: scrub failed; retrying writer once");
            let retry_ctx = DraftPhaseCtx {
                extra_pack_note: Some(DRAFT_SCRUB_RETRY_NOTE),
                router: ctx.router,
                subject: ctx.subject,
                research_pack: ctx.research_pack,
                pack_urls: ctx.pack_urls,
                tools: ctx.tools,
                brief_has_cite: ctx.brief_has_cite,
                session_dir: ctx.session_dir,
            };
            let (retry_response, retry_trace) = run_draft_phase(&retry_ctx).await?;
            draft_trace = draft_trace.clone().accumulate(&retry_trace);
            scrub_and_validate_writer_body(
                &retry_response.message.content,
                ctx.pack_urls,
                paste_subject,
            )?
        }
    };
    body = enforce_required_quotes_on_draft(ctx, paste_subject, body, &mut draft_trace).await?;
    Ok((body, draft_trace))
}

async fn enforce_required_quotes_on_draft(
    ctx: &DraftPhaseCtx<'_>,
    paste_subject: &str,
    body: String,
    draft_trace: &mut CompletionTrace,
) -> Result<String, RagError> {
    use crate::sources::draft_footer::{
        louder_required_quotes_note, missing_quotes_operator_error, missing_required_quoted_spans,
        rework_required_quoted_spans,
    };
    let required = rework_required_quoted_spans(ctx.subject);
    if required.is_empty() {
        return Ok(body);
    }
    let missing = missing_required_quoted_spans(&body, &required);
    if missing.is_empty() {
        return Ok(body);
    }
    let note = louder_required_quotes_note(&missing);
    warn!(
        missing = ?missing,
        "load_draft: required quotes missing; retrying writer once"
    );
    let retry_ctx = DraftPhaseCtx {
        extra_pack_note: Some(note.as_str()),
        router: ctx.router,
        subject: ctx.subject,
        research_pack: ctx.research_pack,
        pack_urls: ctx.pack_urls,
        tools: ctx.tools,
        brief_has_cite: ctx.brief_has_cite,
        session_dir: ctx.session_dir,
    };
    let (retry_response, retry_trace) = run_draft_phase(&retry_ctx).await?;
    *draft_trace = draft_trace.clone().accumulate(&retry_trace);
    let retry_body = scrub_and_validate_writer_body(
        &retry_response.message.content,
        ctx.pack_urls,
        paste_subject,
    )?;
    let still = missing_required_quoted_spans(&retry_body, &required);
    if still.is_empty() {
        return Ok(retry_body);
    }
    Err(RagError::Store(missing_quotes_operator_error(&still)))
}

/// First line / clause of a subject string for subject-paste detection (not the full operator brief).
#[must_use]
pub(crate) fn paste_subject_line(subject: &str) -> String {
    let mut s = subject.trim().to_string();
    for u in extract_http_urls(&s) {
        s = s.replace(&u, " ");
    }
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let clause = s.split([',', ';', '\n']).next().unwrap_or(&s).trim();
    if clause.is_empty() {
        s.chars().take(120).collect()
    } else {
        clause.to_string()
    }
}

/// `LinkedIn` drafts: at least two unique emoji glyphs (same bar as tweets).
///
/// Safety net when the writer under-ships emoji. Weaves missing 🦀/🦉 into the closing
/// sentence — never inserts before a period (that produced `🦀.` paste glue).
fn ensure_draft_emoji_bar(body: &str) -> String {
    if crate::llm::tweet_emoji_ok(body) {
        return body.to_string();
    }
    let mut out = body.trim_end().to_string();
    if !out.contains('🦀') || !out.contains('🦉') {
        let mut tail = String::new();
        if !out.contains('🦀') {
            tail.push('🦀');
        }
        if !out.contains('🦉') {
            if !tail.is_empty() {
                tail.push(' ');
            }
            tail.push('🦉');
        }
        if out.ends_with('.') {
            out.pop();
        }
        if !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(&tail);
    }
    if !crate::llm::tweet_emoji_ok(&out) {
        out.push_str("\n\n🦀 🦉");
    }
    out
}

/// True when prose has emoji glued directly before a period (`🦀.` / `🚀.`).
#[must_use]
pub(crate) fn linkedin_draft_has_emoji_dot_glue(body: &str) -> bool {
    let chars: Vec<char> = body.chars().collect();
    chars
        .windows(2)
        .any(|w| crate::llm::char_is_emoji_like(w[0]) && w[1] == '.')
}

/// Remove a spurious `.` immediately after an emoji (`🚀.` → `🚀`); emoji glyphs stay.
fn strip_spurious_period_after_emoji(body: &str) -> String {
    if !linkedin_draft_has_emoji_dot_glue(body) {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if crate::llm::char_is_emoji_like(c) {
            out.push(c);
            if chars.peek() == Some(&'.') {
                chars.next();
                if chars.peek() == Some(&' ') {
                    out.push(' ');
                    chars.next();
                } else if chars.peek().is_some() {
                    out.push(' ');
                }
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Load phase then draft writer. LOAD may `web_search` / `browse_url`. The writer
/// does not search again when the pack already has publisher URLs.
///
/// Soft `SQLite` RAG hints are NOT injected here: they poisoned drafts with off-topic
/// corpus posts. Models use `corpus_search` as a tool when they need voice/history.
///
/// # Errors
///
/// Returns a [`RagError`] variant for load/draft/LLM/store failure.
pub async fn build_grounded_draft(
    router: &FailoverRouter,
    db_path: &Path,
    embed: &dyn EmbedClient,
    subject: &str,
    tools: Option<&ItcyTools>,
) -> Result<GroundedDraft, RagError> {
    build_grounded_draft_with_cite(router, db_path, embed, subject, "", tools, None).await
}

struct DraftLoadCtx<'a> {
    router: &'a FailoverRouter,
    topic: &'a str,
    instructions: &'a str,
    operator_brief: &'a str,
    tools: Option<&'a ItcyTools>,
    tools_dyn: Option<&'a dyn ToolProvider>,
    session_dir: Option<&'a PathBuf>,
    prefer: Option<&'a str>,
    brief_has_cite: bool,
}

async fn run_draft_load_with_cite(
    ctx: DraftLoadCtx<'_>,
) -> Result<(String, Vec<String>, CompletionTrace), RagError> {
    if let Some(url) = ctx.prefer {
        if let Err(reason) = crate::sources::publisher_url::probe_publisher_url(url).await {
            if crate::sources::publisher_url::cite_probe_soft_fail(&reason) {
                warn!(
                    url = %url,
                    error = %reason,
                    "draft: cite probe soft-fail (bot wall); continuing LOAD"
                );
            } else {
                return Err(RagError::Store(format!(
                    "cite URL not reachable: {url} ({reason})"
                )));
            }
        }
    }
    let (research_pack, mut pack_urls, load_trace) = if let Some(url) = ctx.prefer {
        crate::sources::tweet_load::run_short_cite_load(ctx.operator_brief, url, ctx.tools, true)
            .await?
    } else {
        run_load_phase(
            ctx.router,
            ctx.topic,
            ctx.instructions,
            ctx.tools,
            ctx.tools_dyn,
            ctx.session_dir,
        )
        .await?
    };
    pack_urls = crate::sources::publisher_url::filter_reachable_publisher_urls(pack_urls).await;
    if ctx.brief_has_cite {
        if let Some(url) = ctx.prefer {
            if !pack_urls.iter().any(|u| u == url) {
                pack_urls.insert(0, url.to_string());
            }
        }
    }
    Ok((research_pack, pack_urls, load_trace))
}

/// Like [`build_grounded_draft`] but pins a digest / operator URL as cite slot 1 when set.
///
/// # Errors
///
/// Returns a [`RagError`] variant for load/draft/LLM/store failure.
pub async fn build_grounded_draft_with_cite(
    router: &FailoverRouter,
    db_path: &Path,
    _embed: &dyn EmbedClient,
    topic: &str,
    instructions: &str,
    tools: Option<&ItcyTools>,
    forced_cite_url: Option<&str>,
) -> Result<GroundedDraft, RagError> {
    let operator_brief = crate::sources::tweet_footer::operator_brief(topic, instructions);
    // Prefer Draft ID already allocated by Slack; otherwise allocate here (E2E / tests).
    let session_dir = begin_load_session_dir(tools, db_path, &operator_brief).await;
    let tools_dyn: Option<&dyn ToolProvider> = tools.map(|t| t as &dyn ToolProvider);

    let prefer = resolve_draft_cite_url(forced_cite_url, &operator_brief);
    let brief_has_cite = prefer.is_some();
    let (mut research_pack, pack_urls, load_trace) = run_draft_load_with_cite(DraftLoadCtx {
        router,
        topic,
        instructions,
        operator_brief: &operator_brief,
        tools,
        tools_dyn,
        session_dir: session_dir.as_ref(),
        prefer: prefer.as_deref(),
        brief_has_cite,
    })
    .await?;
    apply_pack_handles(tools, &operator_brief, &mut research_pack);

    checkpoint_building_pack(db_path, tools, &operator_brief, &research_pack, &pack_urls).await;

    let (body, draft_trace) = draft_body_with_scrub_retry(
        &DraftPhaseCtx {
            router,
            subject: &operator_brief,
            research_pack: &research_pack,
            pack_urls: &pack_urls,
            tools,
            brief_has_cite,
            session_dir: session_dir.as_ref(),
            extra_pack_note: None,
        },
        topic.trim(),
    )
    .await?;

    info!(
        draft_model = %draft_trace.model_label(),
        "load_draft: writer done"
    );

    let draft_id = resolve_session_draft_id(tools, db_path).await;
    // Capture SERP/extracted pool before end_session clears tools.session.
    let refill_pool = draft_link_refill_pool(tools, &pack_urls).await;
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
        "load={} | draft={}",
        load_trace.model_label(),
        draft_trace.model_label()
    );
    let mut body = body;
    body = crate::sources::handles::ensure_linkedin_brand_mention(&body);
    body = ensure_body_handles_from_pack(tools, &body, &research_pack);
    let (body, link_options) = finalize_draft_link_options(
        body,
        &pack_urls,
        &refill_pool,
        &operator_brief,
        prefer.as_deref(),
    )
    .await;
    crate::sources::publisher_url::require_link_options_floor(&link_options)
        .map_err(RagError::Store)?;
    let body = crate::sources::draft_footer::compose_draft_message(&body, &draft_id, &link_options);
    info!(draft_id = %draft_id, links = link_options.len(), "load_draft: draft id + links attached");
    Ok(GroundedDraft {
        subject: topic.to_string(),
        body: with_disclosure(&body, &draft_trace),
        draft_id,
        model,
        tokens_in: load_trace
            .prompt_tokens
            .saturating_add(draft_trace.prompt_tokens),
        tokens_out: load_trace
            .completion_tokens
            .saturating_add(draft_trace.completion_tokens),
        source_labels: if pack_urls.is_empty() {
            vec!["load+draft (no verified publisher URL)".into()]
        } else {
            pack_urls
        },
        link_options,
        research_pack,
    })
}

/// Filter pack/refill by subject, pick Link options, promote cite, reachability finalize.
async fn finalize_draft_link_options(
    body: String,
    pack_urls: &[String],
    refill_pool: &[String],
    subject: &str,
    prefer_cite: Option<&str>,
) -> (String, Vec<String>) {
    let filtered_pack =
        crate::sources::corpus_propose::filter_pack_urls_for_subject(pack_urls, subject);
    let mut link_options = crate::sources::draft_footer::pick_link_options(&filtered_pack, &body);
    if let Some(cite) = prefer_cite {
        crate::sources::draft_url::promote_link_option(&mut link_options, cite);
    }
    let filtered_refill =
        crate::sources::corpus_propose::filter_pack_urls_for_subject(refill_pool, subject);
    crate::sources::publisher_url::finalize_reachable_link_options_from_pool(
        &body,
        link_options,
        &filtered_refill,
    )
    .await
}

/// Writer-only draft with a pre-filled Interchouette `ResearchPack` (skips LOAD / SERP).
///
/// # Errors
///
/// Returns a [`RagError`] variant for draft/LLM failure.
pub async fn build_grounded_draft_from_pack(
    router: &FailoverRouter,
    db_path: &Path,
    subject: &str,
    research_pack: &str,
    pack_urls: &[String],
    tools: Option<&ItcyTools>,
) -> Result<GroundedDraft, RagError> {
    let session_dir = begin_load_session_dir(tools, db_path, subject).await;
    let brief_has_cite = crate::sources::tweet_footer::extract_brief_cite(subject).is_some();
    let mut urls: Vec<String> = pack_urls.to_vec();
    urls = crate::sources::publisher_url::filter_reachable_publisher_urls(urls).await;
    let mut research_pack = research_pack.to_string();
    apply_pack_handles(tools, subject, &mut research_pack);
    checkpoint_building_pack(db_path, tools, subject, &research_pack, &urls).await;
    let paste_subject = paste_subject_line(subject);
    let (body, draft_trace) = draft_body_with_scrub_retry(
        &DraftPhaseCtx {
            router,
            subject,
            research_pack: &research_pack,
            pack_urls: &urls,
            tools,
            brief_has_cite,
            session_dir: session_dir.as_ref(),
            extra_pack_note: None,
        },
        &paste_subject,
    )
    .await?;
    let draft_id = resolve_session_draft_id(tools, db_path).await;
    let refill_pool = draft_link_refill_pool(tools, &urls).await;
    end_session_best_effort(
        tools,
        session_dir.as_ref(),
        &session_dir.as_ref().map_or_else(
            || "session_dir: (none)".into(),
            |d| format!("session_dir: {}", d.display()),
        ),
    )
    .await;
    let mut body = body;
    body = crate::sources::handles::ensure_linkedin_brand_mention(&body);
    body = ensure_body_handles_from_pack(tools, &body, &research_pack);
    let prefer = crate::sources::tweet_footer::extract_brief_cite(subject);
    let (body, link_options) =
        finalize_draft_link_options(body, &urls, &refill_pool, subject, prefer.as_deref()).await;
    crate::sources::publisher_url::require_link_options_floor(&link_options)
        .map_err(RagError::Store)?;
    let body = crate::sources::draft_footer::compose_draft_message(&body, &draft_id, &link_options);
    Ok(GroundedDraft {
        subject: subject.to_string(),
        body: with_disclosure(&body, &draft_trace),
        draft_id,
        model: format!("itc-pack | draft={}", draft_trace.model_label()),
        tokens_in: draft_trace.prompt_tokens,
        tokens_out: draft_trace.completion_tokens,
        source_labels: if urls.is_empty() {
            vec!["itc pack (no cite)".into()]
        } else {
            urls
        },
        link_options,
        research_pack,
    })
}

fn prose_word_count(body: &str) -> usize {
    body.lines()
        .filter(|l| {
            let t = l.trim();
            !(t.is_empty()
                || t.starts_with("http://")
                || t.starts_with("https://")
                || t.starts_with("Sources")
                || t.starts_with("Draft ID:")
                || t.starts_with("Draft code:")
                || t.starts_with("Link options")
                || (t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains("http")))
        })
        .flat_map(|l| l.split_whitespace())
        .count()
}

/// True when a `LinkedIn` draft looks like an aerated X tweet (blank-line one-liners).
fn looks_like_x_shaped_linkedin(body: &str) -> bool {
    let prose_lines: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("http://")
                && !l.starts_with("https://")
                && !l.starts_with("Sources")
                && !l.starts_with("Draft ID:")
                && !l.starts_with("Draft code:")
                && !l.starts_with("Link:")
                && !l.starts_with("Cite")
                && !(l.chars().next().is_some_and(|c| c.is_ascii_digit()) && l.contains("http"))
        })
        .collect();
    let hashtag_line = prose_lines.iter().any(|l| {
        l.starts_with('#') || (l.contains('#') && l.split_whitespace().all(|w| w.starts_with('#')))
    });
    if hashtag_line {
        return true;
    }
    if prose_lines.len() < 3 {
        return false;
    }
    let short = prose_lines
        .iter()
        .filter(|l| l.split_whitespace().count() <= 14)
        .count();
    let blank_separated = body.contains("\n\n");
    // Aerated short beats: majority short lines + blank-line rhythm (tweet shape).
    short * 2 >= prose_lines.len() && blank_separated
}

/// True when the model dumped planning / tool narration instead of a `LinkedIn` post.
fn looks_like_writer_scratchpad(body: &str) -> bool {
    let l = body.to_ascii_lowercase();
    l.contains("i will write")
        || l.contains("i'll write")
        || l.contains("corpus search returned")
        || l.contains("draft without summarizing")
        || l.contains("page browse attempt")
        || l.contains("i have enough context")
        || l.contains("keeping voice peer-to-peer")
        || (l.contains("sources: corpus_search") && l.contains("i will"))
        || l.contains("let me look at my corpus")
        || l.contains("let me draft based on")
        || l.contains("the operator subject was")
        || l.contains("so i can't cite")
        || l.contains("i need to write an english linkedin")
        || l.contains("this is the key info i have")
        || l.contains("draft based on what i know")
}

fn normalize_token(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// True when the draft pastes a long contiguous word-run from the paste subject.
/// Structural only: no phrase allow/deny lists.
fn body_copies_operator_subject(body: &str, subject: &str) -> bool {
    const MIN_RUN: usize = 8;
    let subj: Vec<String> = subject
        .split_whitespace()
        .map(normalize_token)
        .filter(|t| !t.is_empty())
        .collect();
    if subj.len() < MIN_RUN {
        return false;
    }
    let bod: Vec<String> = body
        .split_whitespace()
        .map(normalize_token)
        .filter(|t| !t.is_empty())
        .collect();
    if bod.len() < MIN_RUN {
        return false;
    }
    for i in 0..=subj.len() - MIN_RUN {
        let needle = &subj[i..i + MIN_RUN];
        for j in 0..=bod.len() - MIN_RUN {
            if &bod[j..j + MIN_RUN] == needle {
                return true;
            }
        }
    }
    false
}

/// Drop a leading word-run copied from the paste subject when the body opens with the brief lede.
fn strip_leading_subject_paste(body: &str, paste_subject: &str) -> String {
    const MIN_RUN: usize = 8;
    let bod_words: Vec<&str> = body.split_whitespace().collect();
    let subj: Vec<String> = paste_subject
        .split_whitespace()
        .map(normalize_token)
        .filter(|t| !t.is_empty())
        .collect();
    if subj.len() < MIN_RUN || bod_words.len() < MIN_RUN {
        return body.to_string();
    }
    let mut best_strip = 0usize;
    for i in 0..=subj.len() - MIN_RUN {
        let mut k = 0usize;
        while k < bod_words.len() && i + k < subj.len() {
            if normalize_token(bod_words[k]) != subj[i + k] {
                break;
            }
            k += 1;
        }
        if k >= MIN_RUN && k > best_strip {
            best_strip = k;
        }
    }
    if best_strip == 0 {
        return body.to_string();
    }
    bod_words[best_strip..].join(" ")
}

/// End research session if one was opened (always detach product.log tee).
pub(crate) async fn end_session_best_effort(
    tools: Option<&ItcyTools>,
    session_dir: Option<&std::path::PathBuf>,
    note: &str,
) {
    if let Some(t) = tools {
        let n = if session_dir.is_some() {
            note.to_string()
        } else {
            format!("{note} (no session dir)")
        };
        t.end_research_session(&n).await;
    }
}

/// Drop SERP / placeholder URLs from pack / disclosure labels.
pub use crate::sources::url_hygiene::{
    extract_https_urls as extract_http_urls, filter_publisher_urls, is_junk_or_search_url,
};

/// Strip junk / search / shortener URLs from writer output before Slack.
fn scrub_invented_urls(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        let urls = extract_http_urls(line);
        if !urls.is_empty() && urls.iter().all(|u| is_junk_or_search_url(u)) {
            continue;
        }
        if urls.iter().any(|u| is_junk_or_search_url(u)) {
            // Drop only the junk tokens; keep the rest of the line if any prose remains.
            let mut cleaned = line.to_string();
            for u in urls.iter().filter(|u| is_junk_or_search_url(u)) {
                cleaned = cleaned.replace(u, "");
            }
            let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
            if !cleaned.is_empty() {
                out.push_str(&cleaned);
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Drop https URLs that are not in the verified pack (stops invented cites landing in Slack).
fn resolve_draft_cite_url(forced: Option<&str>, subject: &str) -> Option<String> {
    forced
        .map(str::trim)
        .filter(|u| !u.is_empty() && crate::sources::url_hygiene::is_allowed_tweet_cite(u))
        .map(str::to_string)
        .or_else(|| crate::sources::tweet_footer::extract_brief_cite(subject))
}

/// Extra candidates for Link refill after probe drops empty shells / soft 404s.
pub(crate) async fn draft_link_refill_pool(
    tools: Option<&ItcyTools>,
    pack_urls: &[String],
) -> Vec<String> {
    let mut pool = pack_urls.to_vec();
    if let Some(t) = tools {
        for u in t
            .session_extracted_urls()
            .await
            .into_iter()
            .chain(t.session_browsed_urls().await)
        {
            if !pool.iter().any(|x| x == &u) {
                pool.push(u);
            }
        }
    }
    pool
}

fn scrub_urls_outside_pack(body: &str, pack_urls: &[String]) -> String {
    if pack_urls.is_empty() {
        // No verified pack: drop bare https lines so invented cites cannot become Link:1.
        let mut out = String::new();
        for line in body.lines() {
            if crate::sources::draft_url::is_in_post_https_line(line) {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        return out.trim_end().to_string();
    }
    let mut out = String::new();
    for line in body.lines() {
        let urls = extract_http_urls(line);
        if urls.is_empty() {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let mut cleaned = line.to_string();
        let mut dropped = false;
        for u in &urls {
            if !crate::sources::url_hygiene::url_in_allowlist(u, pack_urls) {
                cleaned = cleaned.replace(u, "");
                dropped = true;
            }
        }
        if dropped {
            let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
            if !cleaned.is_empty() {
                out.push_str(&cleaned);
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::client::{LlmClient, LlmResponse, LlmToolDef, LlmUsage};
    use crate::llm::router::{ChainCandidate, TaskChains};
    use crate::sources::embed::MockEmbedClient;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn scrubs_any_linkedin_url_from_draft() {
        let body = "Commentary.\n\nhttps://www.linkedin.com/in/patrick-szymkowiak/\n\nhttps://www.linkedin.com/uas/login\n\nSources: none";
        let scrubbed = scrub_invented_urls(body);
        assert!(!scrubbed.to_ascii_lowercase().contains("linkedin"));
        assert!(scrubbed.contains("Commentary."));
    }

    #[test]
    fn filter_publisher_urls_drops_linkedin() {
        let kept = filter_publisher_urls(&[
            "https://www.linkedin.com/in/x".into(),
            "https://www.rtk-ai.app/team/".into(),
        ]);
        assert_eq!(kept, vec!["https://www.rtk-ai.app/team/".to_string()]);
    }

    #[test]
    fn scrubs_linkedin_login_wall_url() {
        let body = "Thin sources.\n\nhttps://www.linkedin.com/uas/login\n\nSources: none";
        let scrubbed = scrub_invented_urls(body);
        assert!(!scrubbed.contains("uas/login"));
        assert!(scrubbed.contains("Thin sources."));
    }

    #[test]
    fn scrubs_linkedin_short_links() {
        let body = "Nice take.\n\nhttps://lnkd.in/eBxvGMdh\n\nSources: corpus";
        let scrubbed = scrub_invented_urls(body);
        assert!(!scrubbed.contains("lnkd.in"));
        assert!(scrubbed.contains("Nice take."));
    }

    #[test]
    fn filters_google_search_and_example_urls() {
        let urls = vec![
            "https://www.google.com/search?q=x".into(),
            "https://www.example-news-site.com/rtk-ai-labs-ceo-update".into(),
            "https://www.reddit.com/r/opensource/comments/example_fake_thread_".into(),
            "https://labs.sogeti.com/token-tax".into(),
        ];
        let kept = filter_publisher_urls(&urls);
        assert_eq!(kept, vec!["https://labs.sogeti.com/token-tax".to_string()]);
    }

    #[test]
    fn empty_pack_strips_bare_https_lines() {
        let body = "Prose.\n\nhttps://www.pewresearch.org/fake/invented\n\nMore prose.";
        let cleaned = scrub_urls_outside_pack(body, &[]);
        assert!(!cleaned.contains("https://"));
        assert!(cleaned.contains("Prose."));
        assert!(cleaned.contains("More prose."));
    }

    #[test]
    fn forced_cite_wins_over_brief_parse() {
        let cite = resolve_draft_cite_url(
            Some("https://decrypt.co/376271/chatgpt-web-ai-written-pew"),
            "short topic without url",
        );
        assert_eq!(
            cite.as_deref(),
            Some("https://decrypt.co/376271/chatgpt-web-ai-written-pew")
        );
    }

    #[test]
    fn scrubs_example_news_site_and_outside_pack() {
        let body = "Nice post.\n\nhttps://www.example-news-site.com/rtk-ai-labs-ceo-update\n\nSources: none";
        let scrubbed = scrub_invented_urls(body);
        assert!(!scrubbed.contains("example-news-site"));
        assert!(scrubbed.contains("Nice post."));
        let pack = vec!["https://labs.sogeti.com/token-tax".to_string()];
        let with_fake = "Text\n\nhttps://totally-made-up-news.biz/rtk\n\nSources: x";
        let cleaned = scrub_urls_outside_pack(with_fake, &pack);
        assert!(!cleaned.contains("totally-made-up-news.biz"));
        assert!(cleaned.contains("Text"));
    }

    #[test]
    fn scrubs_invented_reddit_example_line() {
        let body = "Nice post.\n\nhttps://www.reddit.com/r/opensource/comments/example_fake_thread_\n\nSources: none";
        let scrubbed = scrub_invented_urls(body);
        assert!(!scrubbed.contains("example_fake"));
        assert!(scrubbed.contains("Nice post."));
    }

    struct OkClient;

    #[async_trait]
    impl LlmClient for OkClient {
        fn provider_id(&self) -> &'static str {
            "mock"
        }

        async fn chat(
            &self,
            messages: &[LlmMessage],
            model: &str,
            _tools: Option<&[LlmToolDef]>,
        ) -> Result<LlmResponse, LlmError> {
            let blob = messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let content = if blob.contains("You are ITCy research")
                || blob.contains("Output ONLY a ResearchPack")
                || blob.contains("You do NOT write the LinkedIn post")
            {
                format!(
                    "## ResearchPack\nsubject: rust async\nsummary: mock pack\n\
candidates:\n- final_url=https://labs.sogeti.com/rust-async | title=Rust | why=on-topic\n\
notes: model={model}"
                )
            } else {
                format!(
                    "DRAFT commentary as ITCy on the news. Rust async runtimes 🦀 keep \
services responsive under load when tasks share a well-tuned executor and backpressure \
is honest about queue depth. Teams that measure latency budgets and cancel abandoned \
work see fewer cascading timeouts in production. Operators who pin versions, surface \
cancellation, and treat the runtime as a product surface ship calmer systems. Shared \
work-stealing queues and bounded channels stop one slow dependency from filling memory. \
When cancellation is polite and metrics name the stall, the room stays boring in the \
best way. Peer notes like this belong on LinkedIn: precise verbs, no slogan mush, and \
room for a single on-topic publisher cite under AI CMO craft for builders who read carefully. \
I'm watching 🦉 how those queues behave under real traffic. \
model={model} ctx_len={}",
                    blob.len()
                )
            };
            Ok(LlmResponse {
                message: LlmMessage::assistant(content),
                finish_reason: "stop".into(),
                usage: Some(LlmUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                }),
            })
        }
    }

    #[test]
    fn looks_like_writer_scratchpad_detects_planning() {
        assert!(looks_like_writer_scratchpad(
            "The corpus search returned hits. I will write a warm LinkedIn-style commentary."
        ));
        assert!(looks_like_writer_scratchpad(
            "The Sogeti page is not found (404), so I can't cite it. The operator subject was RTK. \
Let me look at my corpus search again - this is the key info I have. Let me draft based on what I know."
        ));
        assert!(!looks_like_writer_scratchpad(
            "Leadership moves at RTK AI Labs matter for builders watching open tooling mature."
        ));
    }

    #[test]
    fn looks_like_x_shaped_linkedin_detects_aerated_beats() {
        let x_shaped = "📜 Policy landed in the forge.\n\n\
👀 Reviewers get a line to point at.\n\n\
🦉 watching habits form.\n\n\
🦀 energy for careful diffs.\n\n\
https://example.com/policy";
        assert!(looks_like_x_shaped_linkedin(x_shaped));
        let with_tags = "Builders care about review habit.\n\n#Rust #LLM #OpenSource";
        assert!(looks_like_x_shaped_linkedin(with_tags));
        let linkedin = "When a major code forge publishes a written LLM contribution policy, the interesting part is not the headline - it is the review habit that follows. Maintainers finally get a line they can point at when a contribution leans on a model instead of guessing from vibes.\n\n\
I'm watching how disclose-your-tooling turns into something boring and useful: show the work, keep the tree auditable, skip the press-release fog that says everything and commits to nothing. When the stack is Rust-shaped, careful crates and honest diffs beat slogans for builders who actually ship.\n\n\
https://example.com/policy";
        assert!(!looks_like_x_shaped_linkedin(linkedin));
    }

    #[cfg(itcy_kitchen_prompts)]
    #[test]
    fn draft_system_prompt_uses_linkedin_craft_not_x_beats() {
        let p = draft_system_prompt();
        assert!(p.contains("Form craft - LinkedIn") || p.contains("180-280"));
        assert!(p.contains("Creative CMO studio - LinkedIn") || p.contains("CREATIVE MANDATE"));
        assert!(!p.contains("Form craft - X"));
        assert!(!p.contains("Creative CMO studio - X"));
        assert!(!p.contains("X pattern A"));
        assert!(!p.contains("Target **280**"));
        assert!(!p.contains("120-220"));
        assert!(
            p.contains("2 or 3 unique")
                || p.contains("context-first")
                || p.contains("Context glyph")
                || p.contains("1-3 unique")
                || p.contains("1 to 3 unique")
        );
        assert!(
            FORM_CRAFT_LINKEDIN.contains("ORIGINALITY")
                || CREATIVE_LINKEDIN.contains("ORIGINALITY")
        );
        // No mad-lib teaching skeletons in LinkedIn curricula (they get emitted literally).
        assert!(!FORM_CRAFT_LINKEDIN.contains("[ENTITY]"));
        assert!(!CREATIVE_LINKEDIN.contains("[ENTITY]"));
        assert!(!FORM_CRAFT_LINKEDIN.contains("TEACHING SKELETONS"));
        assert!(!CREATIVE_LINKEDIN.contains("TEACHING SKELETONS"));
        assert!(!FORM_CRAFT_LINKEDIN.contains("When [ENTITY]"));
        assert!(!CREATIVE_LINKEDIN.contains("When [ENTITY]"));
    }

    #[test]
    fn paste_subject_line_uses_first_clause_not_full_brief() {
        let line = paste_subject_line(
            "DoorDash Flux platform, DoorDash has moved engineering agent workloads from laptops to Flux with Firecracker microVMs https://infoq.com/x",
        );
        assert_eq!(line, "DoorDash Flux platform");
        assert!(!line.contains("Firecracker"));
    }

    #[test]
    fn subject_paste_uses_word_runs_not_phrase_lists() {
        let subject = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        assert!(body_copies_operator_subject(
            "Leaders note alpha beta gamma delta epsilon zeta eta theta today.",
            subject
        ));
        assert!(!body_copies_operator_subject(
            "Leadership moves at Acme Labs matter for builders watching tooling mature.",
            subject
        ));
    }

    #[test]
    fn subject_paste_requires_eight_word_run() {
        let subject = "agentic coding 2026 practical guide big";
        assert!(
            !body_copies_operator_subject(
                "🦀 agentic coding 2026 practical guide big is commentary on the release.",
                subject
            ),
            "six-token subject must not trigger paste detection"
        );
        let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        assert!(body_copies_operator_subject(
            "Leaders note alpha beta gamma delta epsilon zeta eta theta today.",
            long
        ));
    }

    #[test]
    fn scrub_aerates_doordash_wall_into_three_paragraphs() {
        use crate::sources::digest_propose_fixtures::{fixture_e_topic, FIXTURE_E_WALL_BODY};
        let topic = fixture_e_topic();
        let out = scrub_and_validate_writer_body(FIXTURE_E_WALL_BODY, &[], &topic)
            .expect("DoorDash wall must pass scrub");
        let blocks: Vec<&str> = out.split("\n\n").collect();
        assert_eq!(blocks.len(), 3, "scrub must aerate LinkedIn wall: {out:?}");
        assert!(
            blocks[0].contains("130K") && blocks[2].contains("The future"),
            "hook and close must land in first and last blocks: {out:?}"
        );
    }

    #[test]
    fn scrub_strips_leading_cite_instruction_leak() {
        let body = "\
cite 📜 @seggwat is a tool that's built for the modern product builder, SaaS teams, \
indie hackers, and product managers who need a way to collect and ship feedback. 🚀 \
The widget stays out of the way while the inbox stays centralized for triage. 🦉\n\n\
Second paragraph keeps builders on one script and one inbox without leaving the workflow. \
That matters when feature requests and bugs land from multiple surfaces at once.";
        let out = scrub_and_validate_writer_body(
            body,
            &[],
            "SeggWat is a feedback platform for SaaS teams, cite https://www.uneed.best/tool/seggwat",
        )
        .expect("cite-prefix leak must scrub clean");
        assert!(
            !out.trim_start().to_ascii_lowercase().starts_with("cite"),
            "must not open with cite: {out}"
        );
        assert!(out.contains("@seggwat") || out.contains("seggwat"), "{out}");
    }

    #[test]
    fn strip_leading_subject_paste_keeps_commentary() {
        use crate::sources::digest_propose_fixtures::{fixture_e_topic, FIXTURE_E_GOOD_BODY};
        let topic = fixture_e_topic();
        let out = scrub_and_validate_writer_body(FIXTURE_E_GOOD_BODY, &[], &topic)
            .expect("digest lede plus article commentary must survive scrub");
        assert!(out.contains("Firecracker"));
        assert!(out.contains("130,000"));
        for banned in crate::sources::digest_propose_fixtures::DELETED_FALLBACK_BANNED {
            assert!(
                !out.to_ascii_lowercase()
                    .contains(&banned.to_ascii_lowercase()),
                "must not contain deleted fallback phrase {banned}: {out}"
            );
        }
    }

    #[cfg(itcy_kitchen_prompts)]
    #[test]
    fn draft_system_forbids_impersonation() {
        let lower = DRAFT_SYSTEM.to_ascii_lowercase();
        assert!(lower.contains("never invent") || lower.contains("forbidden"));
        assert!(DRAFT_SYSTEM.contains("browse_url"));
        assert!(DRAFT_SYSTEM.contains("web_search"));
        assert!(DRAFT_SYSTEM.contains("corpus_search"));
        assert!(DRAFT_SYSTEM.contains("Interchouette ITC"));
        assert!(DRAFT_SYSTEM.contains("em dash") || DRAFT_SYSTEM.contains("Unicode em dash"));
        assert!(DRAFT_SYSTEM.contains("canonical https") || DRAFT_SYSTEM.contains("lnkd.in"));
        assert!(DRAFT_SYSTEM.contains("ResearchPack") || DRAFT_SYSTEM.contains("WRITER"));
        assert!(DRAFT_SYSTEM.contains("SUBJECT LOCK") || DRAFT_SYSTEM.contains("OPERATOR SUBJECT"));
        assert!(
            DRAFT_SYSTEM.contains("cite lock")
                || DRAFT_SYSTEM.contains("do **not** web_search again")
        );
        assert!(DRAFT_SYSTEM.contains("condescending") || DRAFT_SYSTEM.contains("haughty"));
        assert!(DRAFT_SYSTEM.contains("Let's hope") || DRAFT_SYSTEM.contains("moralizing"));
        assert!(
            LOAD_SYSTEM_CORE.contains("News") || LOAD_SYSTEM_CORE.contains("MERGED"),
            "load prompt must mention News/MERGED SERP"
        );
        assert!(!DRAFT_SYSTEM.to_ascii_lowercase().contains("rtk"));
        assert!(!DRAFT_SYSTEM.to_ascii_lowercase().contains("netcup"));
        let with_date = draft_system_prompt();
        assert!(with_date.contains("Today's date"));
        assert!(with_date.contains("Who is who"));
        assert!(with_date.contains("ITCy = this AI"));
        assert!(!with_date.to_ascii_lowercase().contains("rtk"));
        assert!(
            DRAFT_SYSTEM.contains("ai_overview")
                || DRAFT_SYSTEM.contains("AI overview")
                || DRAFT_SYSTEM.to_ascii_lowercase().contains("ai_overview")
        );
        assert!(
            DRAFT_SYSTEM.contains("example*")
                || DRAFT_SYSTEM.contains("example-news-site")
                || DRAFT_SYSTEM.contains("invent"),
            "draft prompt must forbid invented/example hosts"
        );
        assert!(
            LOAD_SYSTEM_CORE.contains("AI_OVERVIEW") || LOAD_SYSTEM_CORE.contains("ai_overview"),
            "load prompt must emphasize AI_OVERVIEW"
        );
        assert!(LOAD_SYSTEM_CORE.contains("ResearchPack"));
        assert!(LOAD_SYSTEM_CORE.contains("web_search"));
        assert!(!LOAD_SYSTEM_CORE.to_ascii_lowercase().contains("rtk"));
    }

    #[test]
    fn extract_urls_from_pack() {
        let urls = extract_http_urls(
            "candidates:\n- final_url=https://example.com/a | x\nhttps://lnkd.in/x ignored short ok",
        );
        assert!(urls.iter().any(|u| u.contains("example.com")));
    }

    #[test]
    fn pack_urls_prefers_serp_extracted_first() {
        let rust = "https://epage.github.io/blog/2026/08/cargo-vision".to_string();
        let fleet =
            "https://gomotive.com/blog/vision-26-ai-automation-future-fleet-operations".to_string();
        let llm_first = pack_urls_from_load(
            std::slice::from_ref(&rust),
            &[],
            std::slice::from_ref(&fleet),
        );
        assert_eq!(llm_first, vec![fleet.clone(), rust.clone()]);
        assert_eq!(llm_first.first().map(String::as_str), Some(fleet.as_str()));
        let serp_only = pack_urls_from_load(
            &[],
            std::slice::from_ref(&rust),
            std::slice::from_ref(&fleet),
        );
        assert_eq!(serp_only, vec![fleet, rust]);
    }

    #[test]
    fn scylla_serp_pack_yields_three_links_with_obvious_news() {
        use crate::sources::draft_footer::pick_link_options;
        let blog2026 =
            "https://www.scylladb.com/2026/08/27/new-rust-driver-for-scylladbs-dynamodb-api/";
        let blog2025 = "https://www.scylladb.com/2025/03/26/scylladb-rust-driver-1-0/";
        let futurum = "https://futurumgroup.com/insights/scylladbs-rust-driver-delivers-58-throughput-gain-for-dynamodb-users/";
        let university = "https://university.scylladb.com/courses/scylla-alternator/";
        let extracted = [blog2026, futurum, university, blog2025]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let pack = pack_urls_from_load(&[], &[], &extracted);
        assert!(pack.len() >= 3, "{pack:?}");
        let opts = pick_link_options(&pack, "");
        assert!(opts.len() >= 3, "{opts:?}");
        assert!(
            opts.iter()
                .any(|u| u.contains("2026/08/27/new-rust-driver")),
            "{opts:?}"
        );
        assert!(
            opts.iter()
                .any(|u| u.contains("futurumgroup.com/insights/scylladbs-rust-driver")),
            "{opts:?}"
        );
        assert!(
            opts.iter().any(|u| u.contains("university.scylladb.com")),
            "{opts:?}"
        );
        assert!(!opts.iter().any(|u| u.contains("2025/03/26")), "{opts:?}");
    }

    #[test]
    fn append_browsed_page_grounds_writer_pack() {
        let mut pack = String::from("## ResearchPack\nsubject: cargo vision\n");
        append_browsed_page_to_pack(
            &mut pack,
            "https://epage.github.io/blog/2026/08/cargo-vision",
            "Cargo is the Rust package manager. This vision is about rustc and crates.io.",
        );
        assert!(pack.contains("## Browsed page"));
        assert!(pack.contains("Rust package manager"));
        let before = pack.len();
        append_browsed_page_to_pack(
            &mut pack,
            "https://epage.github.io/blog/2026/08/cargo-vision",
            "Cargo is the Rust package manager. This vision is about rustc and crates.io.",
        );
        assert_eq!(pack.len(), before);
    }

    #[tokio::test]
    async fn grounded_draft_includes_disclosure_and_context() {
        let dir = TempDir::new().expect("temp");
        let db_path = dir.path().join("s.db");
        let db = SourceDb::open(&db_path).expect("db");
        let sid = db
            .insert_source(&crate::sources::store::InsertSource {
                kind: "personal_feed",
                activity: "post",
                subject: "rust async",
                title: "Rust note",
                url: None,
                raw_text: "Excited about Rust async and tokio runtimes",
                occurred_at: Some("2024-01-01T00:00:00"),
            })
            .expect("src")
            .expect("id");
        let emb = MockEmbedClient
            .embed("m", "Excited about Rust async and tokio runtimes")
            .await
            .expect("emb");
        db.insert_chunk(
            sid,
            "rust async",
            "Excited about Rust async and tokio runtimes",
            &emb,
        )
        .expect("chunk");
        drop(db);

        let mut clients: HashMap<String, Arc<dyn LlmClient>> = HashMap::new();
        clients.insert("mock".into(), Arc::new(OkClient));
        let chains = TaskChains::new().with_chain(
            TaskKind::Draft,
            vec![ChainCandidate::new("mock", "draft-model")],
        );
        let router = FailoverRouter::new(clients, chains);
        let pack = vec![
            "https://alpha.itcy.test/rust-async".into(),
            "https://beta.itcy.test/tokio".into(),
            "https://gamma.itcy.test/queues".into(),
        ];
        let reply = build_grounded_draft_from_pack(
            &router,
            &db_path,
            "rust async",
            "## ResearchPack\nsubject: rust async\nsummary: mock pack\n",
            &pack,
            None,
        )
        .await
        .expect("draft");
        assert!(
            reply.link_options.len() >= crate::sources::publisher_url::LINK_OPTIONS_MIN,
            "floor 3: {:?}",
            reply.link_options
        );
        assert!(reply.body.contains("DRAFT commentary"));
        assert!(reply
            .body
            .contains("Written by AI - ITCy - model mock/draft-model"));
        assert!(reply.body.to_ascii_lowercase().contains("rust"));
        assert!(
            crate::llm::tweet_emoji_ok(&reply.body),
            "writer emoji must survive scrub/compose (got <2 glyphs): {}",
            reply.body
        );
        assert!(
            reply.body.contains('🦉') && reply.body.contains('🦀'),
            "owl + crab must survive the LinkedIn draft pipeline"
        );
        assert!(reply.model.contains("draft=mock/draft-model"));
    }

    #[tokio::test]
    async fn grounded_draft_refuses_fewer_than_three_link_options() {
        let dir = TempDir::new().expect("temp");
        let db_path = dir.path().join("s.db");
        let mut clients: HashMap<String, Arc<dyn LlmClient>> = HashMap::new();
        clients.insert("mock".into(), Arc::new(OkClient));
        let chains = TaskChains::new().with_chain(
            TaskKind::Draft,
            vec![ChainCandidate::new("mock", "draft-model")],
        );
        let router = FailoverRouter::new(clients, chains);
        let err = build_grounded_draft_from_pack(
            &router,
            &db_path,
            "rust async",
            "## ResearchPack\nsubject: rust async\n",
            &[],
            None,
        )
        .await
        .expect_err("Link:0 must fail");
        assert!(err.to_string().contains("at least 3"), "{err}");
        let one = vec!["https://x.com/a/status/1".into()];
        let err = build_grounded_draft_from_pack(
            &router,
            &db_path,
            "rust async",
            "## ResearchPack\nsubject: rust async\n",
            &one,
            None,
        )
        .await
        .expect_err("one link must fail floor");
        assert!(err.to_string().contains("at least 3"), "{err}");
    }

    #[test]
    fn scrub_preserves_woven_emoji_bar() {
        // Keep well above the thin-body fallback threshold so scrub does not replace prose.
        let body = "\
GPUI Component is a bold step for Rust desktop UI 🦀 without Electron overhead for teams \
that already live in native toolchains. Longbridge shipped sixty plus components with data \
tables, dock layouts, and a code editor that leans on Tree-sitter and LSP, all aimed at \
GPU accelerated rendering rather than a browser shell. That framing matters for builders \
who have paid the Electron tax in memory and startup time and want a Rust-first desktop \
stack that still feels modern. The design language borrows from macOS, Windows, and \
shadcn-style component catalogs without pretending the web is the only UI substrate. \
Virtualized lists, native Markdown, and HTML rendering make the library a serious option \
for professional workloads where frame budget and CJK coverage are not optional. \
I'm watching 🦉 how this lands for systems teams that want polish without a second runtime.";
        assert!(
            prose_word_count(body) >= 120,
            "fixture must stay above thin-body fallback"
        );
        let out = scrub_and_validate_writer_body(body, &[], "gpui rust desktop").expect("ok");
        assert!(
            crate::llm::tweet_emoji_ok(&out),
            "scrub must keep >=2 glyphs"
        );
        assert!(out.contains('🦉') && out.contains('🦀'));
        assert_eq!(crate::llm::count_emoji(&out), 2);
    }

    #[test]
    fn scrub_preserves_writer_paragraph_breaks() {
        let body = "\
Sätteri is the Rust-powered Markdown engine Astro shipped for faster builds on real sites. \
Teams parsing MDX on every compile feel the win when the runtime stays native and predictable. 🦀 \
Pulldown-cmark and Oxc keep the parser honest without dragging a JavaScript toolchain along for every page.\n\n\
Native GitHub Flavored Markdown, math, and wikilinks land without a plugin zoo on typical doc sites. \
Builders get fewer moving parts and a cleaner dependency tree when content pipelines already choke on install time. 🚀 \
That matters when CI time is the bottleneck before a release window closes and reviewers want diffs not drama.\n\n\
I'm watching how Astro 7 teams adopt the swap without rewriting every remark plugin they already trust. 🦉 \
Maintainers who measure compile graphs and cache hits will notice the gap first on large content trees.";
        assert!(
            prose_word_count(body) >= 120,
            "fixture must stay above thin-body fallback (got {})",
            prose_word_count(body)
        );
        let out = scrub_and_validate_writer_body(body, &[], "Sätteri Astro Markdown Rust")
            .expect("scrub must not fail clean multi-paragraph prose");
        assert!(
            out.contains("\n\n"),
            "LinkedIn paragraph aeration must survive scrub: {out:?}"
        );
        assert!(
            out.contains("Pulldown-cmark") && out.contains("wikilinks"),
            "both paragraphs must remain: {out}"
        );
    }

    #[test]
    fn scrub_mush_strip_keeps_paragraph_breaks() {
        let body = "\
Sätteri is the Rust-powered Markdown engine Astro shipped for faster builds on real sites. \
Teams parsing MDX on every compile feel the win when the runtime stays native and predictable. 🦀 \
Pulldown-cmark and Oxc keep the parser honest without dragging a JavaScript toolchain along for every page.\n\n\
Native GitHub Flavored Markdown, math, and wikilinks land without a plugin zoo on typical doc sites. \
It's not about which parser wins a keynote slide; it's about compile time on real production sites. 🚀 \
That matters when CI time is the bottleneck before a release window closes and reviewers want diffs not drama.\n\n\
I'm watching how Astro 7 teams adopt the swap without rewriting every remark plugin they already trust. 🦉 \
Maintainers who measure compile graphs and cache hits will notice the gap first on large content trees.";
        assert!(
            prose_word_count(body) >= 120,
            "fixture must stay above thin-body fallback (got {})",
            prose_word_count(body)
        );
        let out = scrub_and_validate_writer_body(body, &[], "Sätteri Astro Markdown Rust")
            .expect("mush salvage must deliver open prose, not Err");
        assert!(
            !crate::sources::corpus_propose::body_has_slogan_mush(&out),
            "mush must be stripped: {out}"
        );
        assert!(
            out.contains("\n\n"),
            "mush strip must not collapse LinkedIn paragraphs to one blob: {out:?}"
        );
        assert!(
            out.contains("Pulldown-cmark") && out.contains("Astro 7"),
            "clean paragraphs must remain after mush strip: {out}"
        );
    }

    #[test]
    fn strip_spurious_period_after_emoji_normalizes_model_x_beat_habit() {
        let raw = "maintainable stack. 🚀. But here's the kicker";
        let out = strip_spurious_period_after_emoji(raw);
        assert!(
            !linkedin_draft_has_emoji_dot_glue(&out),
            "must remove period after emoji, not the emoji: {out:?}"
        );
        assert!(out.contains('🚀'), "emoji must remain: {out:?}");
        assert!(out.contains("stack. 🚀 But"), "{out}");
    }

    // Regression: strip_spurious_period_after_emoji must NOT touch periods that
    // follow normal words (not emoji). Reported: sentences like "flickers out"
    // and "trust anchor" were losing their terminal period even though the bug
    // was actually Qwen omitting them; confirm the function itself is innocent.
    #[test]
    fn strip_spurious_period_does_not_eat_sentence_ending_periods() {
        let cases = [
            "even when the AI it's built on flickers out.",
            "It's the kind of edge-case thinking that turns a tool into a trust anchor.",
            "it depends on being in control.",
            "they just need to run their code.",
            "That's the quiet power of local AI.",
        ];
        for case in &cases {
            let out = strip_spurious_period_after_emoji(case);
            assert_eq!(
                out, *case,
                "period must be untouched when no emoji precedes it: {out:?}"
            );
        }
    }

    // Regression: the function also must not drop periods mid-sentence.
    #[test]
    fn strip_spurious_period_preserves_all_non_emoji_periods() {
        let body =
            "No cloud dependency, no waiting for a server to reboot. Just code that keeps going.";
        assert_eq!(strip_spurious_period_after_emoji(body), body);
    }

    #[test]
    fn ensure_draft_emoji_bar_never_emits_emoji_dot_glue() {
        let thin = "\
First paragraph names Sätteri and why Astro teams care about compile time on content-heavy sites. \
Teams shipping docs at scale need predictable parse cost before they touch routing or auth layers.\n\n\
Second paragraph names native GFM, math, and wikilinks without a plugin zoo on typical doc sites. \
That keeps dependency graphs smaller when CI already runs linters, tests, and type checks on every push.";
        let out = ensure_draft_emoji_bar(thin);
        assert!(
            !linkedin_draft_has_emoji_dot_glue(&out),
            "scrub must not glue emoji before a period: {out:?}"
        );
        assert!(
            crate::llm::tweet_emoji_ok(&out),
            "emoji bar must still pass: {out}"
        );
    }

    #[test]
    fn scrub_leaves_writer_woven_emojis_untouched_when_bar_met() {
        let body = "\
Sätteri is the Rust-powered Markdown engine Astro shipped for faster builds on real sites. \
Teams parsing MDX on every compile feel the win when the runtime stays native and predictable. 🦀 \
Pulldown-cmark and Oxc keep the parser honest without dragging a JavaScript toolchain along for every page.\n\n\
Native GitHub Flavored Markdown, math, and wikilinks land without a plugin zoo on typical doc sites. \
Builders get fewer moving parts and a cleaner dependency tree when content pipelines already choke on install time. 🚀 \
That matters when CI time is the bottleneck before a release window closes and reviewers want diffs not drama.\n\n\
I'm watching how Astro 7 teams adopt the swap without rewriting every remark plugin they already trust. 🦉 \
Maintainers who measure compile graphs and cache hits will notice the gap first on large content trees.";
        let out =
            scrub_and_validate_writer_body(body, &[], "Sätteri Astro Markdown Rust").expect("ok");
        assert!(
            !linkedin_draft_has_emoji_dot_glue(&out),
            "writer-woven emoji must not get dot glue: {out:?}"
        );
        assert!(out.contains('🦀') && out.contains('🦉') && out.contains('🚀'));
    }

    #[test]
    fn ensure_draft_emoji_bar_preserves_paragraph_breaks() {
        let body = "\
First paragraph names the engine and why builders care about compile time on content-heavy sites. \
Teams shipping docs at scale need predictable parse cost before they touch routing or auth layers.\n\n\
Second paragraph names native GFM, math, and wikilinks without a plugin zoo on typical doc sites. \
That keeps dependency graphs smaller when CI already runs linters, tests, and type checks on every push.";
        let out = ensure_draft_emoji_bar(body);
        assert!(
            out.contains("\n\n"),
            "emoji injection must not collapse LinkedIn paragraphs: {out:?}"
        );
        assert!(
            crate::llm::tweet_emoji_ok(&out),
            "emoji bar must be satisfied: {out}"
        );
    }

    #[test]
    fn scrub_salvages_slogan_mush_without_failing() {
        let body = "Cloudflare published a durable-object pricing change that shifts how edge \
state bills per request instead of hiding cost inside bandwidth lines. Teams running WebSockets \
and small session stores on Workers need to re-read the meter before the next deploy window \
closes. The update also clarifies idle retention and cross-region replication surcharges that \
were easy to miss in older docs. For builders shipping multiplayer backends or sync engines on \
the edge, the headline is simpler billing math with fewer surprise overages during traffic \
spikes. Operators should map their current object counts and egress before the grace period \
ends. Finance teams want line-item clarity; platform teams want predictable caps; product teams \
want fewer midnight pages when a demo goes viral. None of that requires slogan framing. \
Still, the draft must not use template contrast lines. 🦀 The debate is not about edge versus \
core; it's not about which cloud wins mindshare in a keynote slide.";
        assert!(
            prose_word_count(body) >= 120,
            "fixture must stay above thin-body fallback"
        );
        let out = scrub_and_validate_writer_body(body, &[], "Cloudflare durable object pricing")
            .expect("mush must coerce, not fail");
        assert!(
            !crate::sources::corpus_propose::body_has_slogan_mush(&out),
            "salvaged body must not keep mush"
        );
        assert!(prose_word_count(&out) >= 80);
    }

    #[test]
    fn scrub_injects_emoji_when_writer_omits_them() {
        let thin = "Teams shipping internal agent platforms need honest audit trails and scoped credentials before they trust automated code review at scale.";
        let out = scrub_and_validate_writer_body(thin, &[], "DoorDash Flux agent platform")
            .expect("short writer prose must pass scrub");
        assert!(
            crate::llm::tweet_emoji_ok(&out),
            "scrub must weave emoji bar onto writer text: {out}"
        );
        assert!(out.contains('🦉') && out.contains('🦀'), "{out}");
        for banned in crate::sources::digest_propose_fixtures::DELETED_FALLBACK_BANNED {
            assert!(
                !out.to_ascii_lowercase()
                    .contains(&banned.to_ascii_lowercase()),
                "must not inject deleted fallback: {out}"
            );
        }
    }

    #[test]
    fn scratchpad_on_cite_path_returns_err_not_static_prose() {
        use crate::sources::digest_propose_fixtures::{fixture_c_brief, FIXTURE_C_BAD_BODY};
        let pack = ["https://www.infoq.com/news/2026/08/aws-bench-agent-evaluation".to_string()];
        let paste = paste_subject_line(&fixture_c_brief());
        let err = scrub_and_validate_writer_body(FIXTURE_C_BAD_BODY, &pack, &paste)
            .expect_err("scratchpad must fail scrub, not inject static prose");
        assert!(err.to_string().contains("planning monologue"), "{err}");
    }

    #[test]
    fn failed_scrub_subject_only_returns_err_not_static_prose() {
        use crate::sources::digest_propose_fixtures::{fixture_e_topic, FIXTURE_E_LEDE};
        let topic = fixture_e_topic();
        let err = scrub_and_validate_writer_body(FIXTURE_E_LEDE, &[], &topic)
            .expect_err("subject lede without commentary must fail scrub");
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("pasted the subject") || msg.contains("empty"),
            "{err}"
        );
        for banned in crate::sources::digest_propose_fixtures::DELETED_FALLBACK_BANNED {
            assert!(
                !msg.contains(&banned.to_ascii_lowercase()),
                "error must not contain deleted fallback phrase: {err}"
            );
        }
    }

    #[test]
    fn scratchpad_without_cite_still_errors() {
        let err = scrub_and_validate_writer_body(
            "The corpus search returned hits. I will write a LinkedIn post.",
            &[],
            "some subject",
        )
        .expect_err("non-cite path keeps hard fail on monologue");
        assert!(err.to_string().contains("planning monologue"), "{err}");
    }

    #[test]
    fn brief_has_cite_when_digest_url_present() {
        use crate::sources::digest_propose_fixtures::fixture_b_brief;
        let brief = fixture_b_brief();
        assert!(crate::sources::tweet_footer::extract_brief_cite(&brief).is_some());
    }

    #[test]
    fn fixture_a_brief_has_x_cite_for_subject_lock() {
        use crate::sources::digest_propose_fixtures::fixture_a_brief;
        let cite = crate::sources::tweet_footer::extract_brief_cite(&fixture_a_brief());
        assert!(cite.is_some());
        assert!(cite.unwrap().contains("x.com"));
    }

    #[test]
    fn fixture_b_bad_body_tokens_off_topic_for_open_weights() {
        use crate::sources::digest_propose_fixtures::{fixture_b_brief, FIXTURE_B_BAD_BODY};
        let brief = fixture_b_brief().to_ascii_lowercase();
        let body = FIXTURE_B_BAD_BODY.to_ascii_lowercase();
        assert!(brief.contains("open weights"));
        assert!(body.contains("oxish"));
        assert!(!body.contains("open weights"));
    }
}

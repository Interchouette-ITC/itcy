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

use crate::prompts::{draft_pack_note, draft_user_message, fallback_commentary, load_user_message};

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
            if let Some(u) = prefer_support_url(&pack_urls).map(str::to_string) {
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

/// Pack cites: LOAD text + browsed pages. SERP leftovers only if both are empty.
fn pack_urls_from_load(
    from_pack: &[String],
    browsed: &[String],
    extracted: &[String],
) -> Vec<String> {
    let mut pack_urls = filter_publisher_urls(from_pack);
    merge_urls_cap(&mut pack_urls, browsed.iter().cloned(), 3);
    if pack_urls.is_empty() {
        merge_urls_cap(&mut pack_urls, extracted.iter().cloned(), 3);
    }
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

pub(crate) async fn run_load_phase(
    router: &FailoverRouter,
    subject: &str,
    tools: Option<&ItcyTools>,
    tools_dyn: Option<&dyn ToolProvider>,
    session_dir: Option<&PathBuf>,
) -> Result<(String, Vec<String>, CompletionTrace), RagError> {
    log_pipeline_banner("LOAD (research pack)");
    log_pipeline_step("LOAD llm");
    info!(subject = %subject, "load_draft: start");
    info!("load_draft: calling LLM (tool loop; may take a while with no further logs)");

    let load_messages = vec![
        LlmMessage::system(load_system_prompt()),
        LlmMessage::user(load_user_message(subject)),
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
            empty_research_pack(subject)
        } else {
            raw
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

async fn run_draft_phase(
    router: &FailoverRouter,
    subject: &str,
    research_pack: &str,
    pack_urls: &[String],
    tools: Option<&ItcyTools>,
    brief_has_cite: bool,
    session_dir: Option<&PathBuf>,
) -> Result<(LlmResponse, CompletionTrace), RagError> {
    if let Some(t) = tools {
        if brief_has_cite {
            t.set_draft_subject_https_writer_policy().await;
        } else {
            t.set_draft_policy(pack_urls).await;
        }
    }

    let tools_dyn: Option<&dyn ToolProvider> = if brief_has_cite {
        None
    } else {
        tools.map(|t| t as &dyn ToolProvider)
    };

    log_pipeline_banner("DRAFT (writer)");

    let pack_note = draft_pack_note(pack_urls.is_empty(), brief_has_cite);
    let user = if brief_has_cite {
        crate::prompts::draft_user_message_subject_https(research_pack, pack_note, subject)
    } else {
        draft_user_message(research_pack, pack_note, subject)
    };
    let draft_messages = vec![
        LlmMessage::system(draft_system_prompt()),
        LlmMessage::user(user),
    ];
    match router
        .complete_with_tools(TaskKind::Draft, &draft_messages, tools_dyn, MAX_TOOL_ROUNDS)
        .await
    {
        Ok(v) => Ok(v),
        Err(e) => {
            end_session_best_effort(tools, session_dir, &format!("draft failed: {e}")).await;
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
    subject: &str,
    brief_has_cite: bool,
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
    if looks_like_writer_scratchpad(&body) {
        if brief_has_cite {
            let primary = pack_urls.first().cloned();
            warn!(
                prose_words = prose_word_count(&body),
                "load_draft: planning/monologue on cite path; injecting subject-safe fallback prose"
            );
            body = fallback_subject_commentary(subject, primary.as_deref());
        } else {
            warn!(
                prose_words = prose_word_count(&body),
                "load_draft: writer returned planning/monologue; refusing to post"
            );
            return Err(RagError::Store(
                "writer returned planning monologue instead of a LinkedIn post".into(),
            ));
        }
    }
    let words = prose_word_count(&body);
    let x_shaped = looks_like_x_shaped_linkedin(&body);
    if words < 120 || x_shaped || body_copies_operator_subject(&body, subject) {
        let primary = pack_urls.first().cloned();
        warn!(
            prose_words = words,
            x_shaped,
            subject_paste = body_copies_operator_subject(&body, subject),
            "load_draft: thin/x-shaped/subject-paste writer body; injecting subject-safe fallback prose"
        );
        body = fallback_subject_commentary(subject, primary.as_deref());
    }
    body = ensure_draft_emoji_bar(&body);
    Ok(body)
}

/// `LinkedIn` drafts: at least two unique emoji glyphs (same bar as tweets).
fn ensure_draft_emoji_bar(body: &str) -> String {
    if crate::llm::tweet_emoji_ok(body) {
        return body.to_string();
    }
    let mut out = body.trim_end().to_string();
    if !out.contains('🦀') {
        // Weave crab near the first sentence break when missing.
        if let Some(i) = out.find(". ") {
            out.insert_str(i + 1, " 🦀");
        } else {
            out.push_str(" 🦀");
        }
    }
    if !out.contains('🦉') {
        if let Some(i) = out.rfind('.') {
            out.insert_str(i, " 🦉");
        } else {
            out.push_str(" 🦉");
        }
    }
    // Still short of two unique glyphs: force the signature pair.
    if !crate::llm::tweet_emoji_ok(&out) {
        out.push_str("\n\n🦉 🦀");
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
    build_grounded_draft_with_cite(router, db_path, embed, subject, tools, None).await
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
    subject: &str,
    tools: Option<&ItcyTools>,
    forced_cite_url: Option<&str>,
) -> Result<GroundedDraft, RagError> {
    // Prefer Draft ID already allocated by Slack; otherwise allocate here (E2E / tests).
    let session_dir = begin_load_session_dir(tools, db_path, subject).await;
    let tools_dyn: Option<&dyn ToolProvider> = tools.map(|t| t as &dyn ToolProvider);

    // Same rule as tweets: https already in the operator brief is the cite.
    // Free LOAD web_search on a short stub (e.g. truncated digest subject) attaches
    // off-topic SERP rows and the writer follows them.
    let prefer = resolve_draft_cite_url(forced_cite_url, subject);
    let brief_has_cite = prefer.is_some();
    if let Some(url) = prefer.as_deref() {
        if let Err(reason) = crate::sources::publisher_url::probe_publisher_url(url).await {
            return Err(RagError::Store(format!(
                "cite URL not reachable: {url} ({reason})"
            )));
        }
    }
    let (mut research_pack, mut pack_urls, load_trace) = if let Some(url) = prefer.as_deref() {
        crate::sources::tweet_load::run_short_cite_load(subject, url, tools, true).await?
    } else {
        run_load_phase(router, subject, tools, tools_dyn, session_dir.as_ref()).await?
    };
    pack_urls = crate::sources::publisher_url::filter_reachable_publisher_urls(pack_urls).await;
    if brief_has_cite {
        if let Some(url) = prefer.as_deref() {
            if !pack_urls.iter().any(|u| u == url) {
                pack_urls.insert(0, url.to_string());
            }
        }
    }
    apply_pack_handles(tools, subject, &mut research_pack);

    checkpoint_building_pack(db_path, tools, subject, &research_pack, &pack_urls).await;

    let (draft_response, draft_trace) = run_draft_phase(
        router,
        subject,
        &research_pack,
        &pack_urls,
        tools,
        brief_has_cite,
        session_dir.as_ref(),
    )
    .await?;

    info!(
        draft_model = %draft_trace.model_label(),
        "load_draft: writer done"
    );

    let draft_id = resolve_session_draft_id(tools, db_path).await;
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
    let mut body = scrub_and_validate_writer_body(
        &draft_response.message.content,
        &pack_urls,
        subject,
        brief_has_cite,
    )?;
    body = crate::sources::handles::ensure_linkedin_brand_mention(&body);
    body = ensure_body_handles_from_pack(tools, &body, &research_pack);
    let mut link_options = crate::sources::draft_footer::pick_link_options(&pack_urls, &body);
    if let Some(cite) = prefer.as_deref() {
        // Digest / operator cite wins Link:1 (including X status URLs).
        crate::sources::draft_url::promote_link_option(&mut link_options, cite);
    }
    let refill_pool = draft_link_refill_pool(tools, &pack_urls).await;
    (body, link_options) =
        crate::sources::publisher_url::finalize_reachable_link_options_from_pool(
            &body,
            link_options,
            &refill_pool,
        )
        .await;
    let body = crate::sources::draft_footer::compose_draft_message(&body, &draft_id, &link_options);
    info!(draft_id = %draft_id, links = link_options.len(), "load_draft: draft id + links attached");
    Ok(GroundedDraft {
        subject: subject.to_string(),
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
    let (draft_response, draft_trace) = run_draft_phase(
        router,
        subject,
        &research_pack,
        &urls,
        tools,
        brief_has_cite,
        session_dir.as_ref(),
    )
    .await?;
    let draft_id = resolve_session_draft_id(tools, db_path).await;
    end_session_best_effort(
        tools,
        session_dir.as_ref(),
        &session_dir.as_ref().map_or_else(
            || "session_dir: (none)".into(),
            |d| format!("session_dir: {}", d.display()),
        ),
    )
    .await;
    let mut body = scrub_and_validate_writer_body(
        &draft_response.message.content,
        &urls,
        subject,
        brief_has_cite,
    )?;
    body = crate::sources::handles::ensure_linkedin_brand_mention(&body);
    body = ensure_body_handles_from_pack(tools, &body, &research_pack);
    let mut link_options = crate::sources::draft_footer::pick_link_options(&urls, &body);
    if let Some(cite) = crate::sources::tweet_footer::extract_brief_cite(subject) {
        crate::sources::draft_url::promote_link_option(&mut link_options, &cite);
    }
    let refill_pool = draft_link_refill_pool(tools, &urls).await;
    (body, link_options) =
        crate::sources::publisher_url::finalize_reachable_link_options_from_pool(
            &body,
            link_options,
            &refill_pool,
        )
        .await;
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

/// True when the draft pastes a long contiguous word-run from the operator subject.
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

/// Compact topic for fallback: first clause, word-capped (no phrase lists, no URLs).
#[must_use]
fn short_topic_for_fallback(subject: &str) -> String {
    let mut s = subject.trim().to_string();
    for u in extract_http_urls(&s) {
        s = s.replace(&u, " ");
    }
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let clause = s.split([',', ';', '\n']).next().unwrap_or(&s).trim();
    let words: Vec<_> = clause.split_whitespace().take(10).collect();
    let t = words.join(" ");
    if t.is_empty() {
        s.chars().take(60).collect()
    } else {
        t
    }
}

/// Subject-locked commentary when the writer returns empty / URL-only / leaked text.
fn fallback_subject_commentary(subject: &str, primary: Option<&str>) -> String {
    let topic = short_topic_for_fallback(subject);
    let mut out = fallback_commentary(&topic);
    if let Some(u) = primary {
        out.push_str("\n\n");
        out.push_str(u);
    }
    out
}

/// Prefer a news/blog-like support URL over /team or GitHub for auto-browse + cite.
/// Among strong scores, keep pack/SERP order (first strong URL wins).
fn prefer_support_url(urls: &[String]) -> Option<&str> {
    let scored = |u: &str| -> i32 {
        let l = u.to_ascii_lowercase();
        if l.contains("github.com") || l.contains("/issues/") {
            return 0;
        }
        if l.contains("/team") || l.ends_with("/team/") {
            return 1;
        }
        if l.contains("crunchbase.com") {
            return 2;
        }
        // On-topic RTK / Sogeti analysis beats generic CEO listicles.
        if l.contains("token-tax")
            || l.contains("token_tax")
            || l.contains("/rtk")
            || (l.contains("sogeti") && (l.contains("rtk") || l.contains("token")))
        {
            return 8;
        }
        if l.contains("/blog")
            || l.contains("article")
            || l.contains("labs.")
            || l.contains("news")
            || l.contains("token")
        {
            return 5;
        }
        3
    };
    for u in urls {
        if scored(u) >= 5 {
            return Some(u.as_str());
        }
    }
    urls.iter().max_by_key(|u| scored(u)).map(String::as_str)
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
async fn draft_link_refill_pool(tools: Option<&ItcyTools>, pack_urls: &[String]) -> Vec<String> {
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
    fn prefer_support_url_keeps_serp_order_among_strong() {
        let urls = vec![
            "https://labs.sogeti.com/the-hidden-cost-of-ai-coding-how-rtk-helps-developers-defeat-the-token-tax-part-2/"
                .into(),
            "https://blog.mean.ceo/ai-industry-trends-july-2026".into(),
        ];
        assert_eq!(
            prefer_support_url(&urls),
            Some(urls[0].as_str()),
            "first strong on-topic URL should win over later blog listicle"
        );
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
    fn short_topic_takes_first_clause() {
        let t = short_topic_for_fallback(
            "acme labs new CEO, then a long briefing with many extra words about research",
        );
        assert_eq!(t, "acme labs new CEO");
        assert!(!t.contains("briefing"));
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
    fn pack_urls_keep_load_cite_not_serp_homonyms() {
        let rust = "https://epage.github.io/blog/2026/08/cargo-vision".to_string();
        let fleet =
            "https://gomotive.com/blog/vision-26-ai-automation-future-fleet-operations".to_string();
        let pack = pack_urls_from_load(
            std::slice::from_ref(&rust),
            &[],
            std::slice::from_ref(&fleet),
        );
        assert_eq!(pack, vec![rust.clone()]);
        assert!(!pack.iter().any(|u| u.contains("gomotive")));
        let recovered = pack_urls_from_load(
            &[],
            std::slice::from_ref(&rust),
            std::slice::from_ref(&fleet),
        );
        assert_eq!(recovered, vec![rust]);
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
        let chains = TaskChains::new()
            .with_chain(
                TaskKind::Load,
                vec![ChainCandidate::new("mock", "load-model")],
            )
            .with_chain(
                TaskKind::Draft,
                vec![ChainCandidate::new("mock", "draft-model")],
            );
        let router = FailoverRouter::new(clients, chains);
        let reply = build_grounded_draft(&router, &db_path, &MockEmbedClient, "rust async", None)
            .await
            .expect("draft");
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
        assert!(reply.model.contains("load=mock/load-model"));
        assert!(reply.model.contains("draft=mock/draft-model"));
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
        let out =
            scrub_and_validate_writer_body(body, &[], "gpui rust desktop", false).expect("ok");
        assert!(
            crate::llm::tweet_emoji_ok(&out),
            "scrub must keep >=2 glyphs"
        );
        assert!(out.contains('🦉') && out.contains('🦀'));
        assert_eq!(crate::llm::count_emoji(&out), 2);
    }

    #[test]
    fn scrub_injects_emoji_when_fallback_or_writer_omits_them() {
        let pack = ["https://x.com/a/status/1".to_string()];
        let out =
            scrub_and_validate_writer_body("short", &pack, "Rust Glancer LSP", false).expect("ok");
        assert!(
            crate::llm::tweet_emoji_ok(&out),
            "fallback/scrub must force emoji bar: {out}"
        );
        assert!(out.contains('🦉') && out.contains('🦀'), "{out}");
    }

    #[test]
    fn scratchpad_on_cite_path_uses_fallback_not_err() {
        use crate::sources::digest_propose_fixtures::{fixture_c_brief, FIXTURE_C_BAD_BODY};
        let pack = ["https://www.infoq.com/news/2026/08/aws-bench-agent-evaluation".to_string()];
        let out =
            scrub_and_validate_writer_body(FIXTURE_C_BAD_BODY, &pack, &fixture_c_brief(), true)
                .expect("cite path must deliver fallback prose, not Err");
        assert!(
            !out.to_ascii_lowercase().contains("corpus search returned"),
            "fallback must not keep monologue: {out}"
        );
        assert!(
            out.to_ascii_lowercase().contains("aws")
                || out.to_ascii_lowercase().contains("bench")
                || out.to_ascii_lowercase().contains("agent"),
            "fallback should stay on brief topic: {out}"
        );
    }

    #[test]
    fn scratchpad_without_cite_still_errors() {
        let err = scrub_and_validate_writer_body(
            "The corpus search returned hits. I will write a LinkedIn post.",
            &[],
            "some subject",
            false,
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

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Slack runtime wiring: secrets resolution and event handling.

use crate::bat::store::{
    status, stored_building_stub, stored_from_payload, DraftPayload, DraftStore,
};
use crate::bat::submit::{accept_draft, ensure_open_for_edit, retry_bat};
use crate::config::Config;
use crate::llm::router::TaskKind;
use crate::llm::FailoverRouter;
use crate::memory::{MemoryDb, StoredMessage};
use crate::slack::api::{post_digest_channel, post_message, post_propose_batch};
use crate::slack::chat::{build_runtime_reply, llm_unavailable_reply};
use crate::slack::commands::{
    classify_channel_text, compose_operator_brief, find_draft_id_in_text, help_text,
    parse_slash_command, slash_command_name, slash_immediate_ack, slash_reply_headline,
    status_text, ChannelTextKind, OperatorCommand, SlashCommandOutcome, StatusSnapshot,
};
use crate::slack::events::ParsedEvent;
use crate::slack::filter::is_channel_allowed;
use crate::slack::propose::ProposeBatch;
use crate::slack::welcome::welcome_text;
use crate::sources::digest::{build_daily_digest, digest_slack_post, format_digest_slack};
use crate::sources::draft_footer::{
    compose_draft_message, linkedin_manual_paste_message, slack_paste_safe_linkedin_message,
};
use crate::sources::draft_url::{
    extract_in_post_url, footer_start, promote_link_option, resolve_url_choice,
    set_single_in_post_url, UrlChoice,
};
use crate::sources::enrich::{
    enrich_linkedin_url_at, EnrichManualResult, EnrichManualVia, TorSocksFetcher,
    DEFAULT_TOR_CONTROL, DEFAULT_TOR_SOCKS,
};
use crate::sources::export::{import_linkedin_export, resolve_export_path};
use crate::sources::html::content_preview;
use crate::sources::ingest::{ingest_url, HttpThenPublicPlaywright};
use crate::sources::portability::{
    import_portability_corpus, resolve_linkedin_access_token, HttpPortabilityClient,
};
use crate::sources::rag::build_grounded_draft_with_cite;
use crate::sources::rag::RagError;
use crate::sources::rework::rework_stored_draft;
use crate::sources::scrape_cache::{resolve_scrape_cache_path, ScrapeCache};
use crate::sources::store::SourceDb;
use crate::sources::EmbedClient;
use crate::tools::{operator_draft_status_reply, ItcyTools};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, warn};

/// Blank line + banner for each incoming #itcy message (visible in screen logs).
fn log_slack_incoming(text: &str) {
    let preview: String = text.chars().take(240).collect();
    crate::sources::rag::log_pipeline_banner("Slack #itcy");
    info!(text = %preview, "slack: incoming message");
}

/// Resolved Slack tokens + channel allowlist for Socket Mode.
#[derive(Debug, Clone)]
pub struct SlackRuntimeConfig {
    pub bot_token: String,
    pub app_token: String,
    pub channel_id: String,
    /// `#daily-digest` channel (empty when unset).
    pub daily_digest_channel_id: String,
    pub bind: String,
    pub max_context_messages: u32,
    pub state_db_path: PathBuf,
    pub linkedin_export_dir: PathBuf,
}

/// Live runtime handle shared with the Socket Mode loop.
#[derive(Clone)]
pub struct SlackRuntime {
    pub config: SlackRuntimeConfig,
    pub memory: Arc<Mutex<MemoryDb>>,
    pub sources: Arc<Mutex<SourceDb>>,
    pub embed: Arc<dyn EmbedClient>,
    pub slack_connected: Arc<Mutex<bool>>,
    pub llm: Arc<FailoverRouter>,
    pub tools: Arc<ItcyTools>,
}

impl SlackRuntime {
    /// Opens memory/sources. `tools` must already own a long-lived host browser (binary boot).
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` with an operator-facing message when validation or lookup fails.
    pub fn new(
        config: SlackRuntimeConfig,
        llm: Arc<FailoverRouter>,
        embed: Arc<dyn EmbedClient>,
        tools: Arc<ItcyTools>,
    ) -> Result<Self, String> {
        let memory = MemoryDb::open(&config.state_db_path).map_err(|e| e.to_string())?;
        let sources = SourceDb::open(&config.state_db_path).map_err(|e| e.to_string())?;
        info!(
            "slack: tool provider attached (corpus_search + browse_url; Chrome from binary boot)"
        );
        let runtime = Self {
            config,
            memory: Arc::new(Mutex::new(memory)),
            sources: Arc::new(Mutex::new(sources)),
            embed,
            slack_connected: Arc::new(Mutex::new(false)),
            llm,
            tools,
        };
        runtime.try_import_linkedin_export();
        runtime.try_import_linkedin_portability();
        Ok(runtime)
    }

    fn try_import_linkedin_export(&self) {
        let path = resolve_export_path(self.config.linkedin_export_dir.to_string_lossy().as_ref());
        if !path.exists() {
            info!(
                path = %path.display(),
                "sources: LinkedIn export path missing (drop official zip/dir when ready)"
            );
            return;
        }
        // Skip empty placeholder dirs (only README).
        let has_data = std::fs::read_dir(&path).is_ok_and(|rd| {
            rd.filter_map(Result::ok).any(|e| {
                let path = e.path();
                path.extension().is_some_and(|x| {
                    x.eq_ignore_ascii_case("csv")
                        || x.eq_ignore_ascii_case("json")
                        || x.eq_ignore_ascii_case("zip")
                }) || path.is_dir()
            })
        });
        if !has_data {
            info!(
                path = %path.display(),
                "sources: no LinkedIn export data yet (legal zip refresh path)"
            );
            return;
        }
        let existing = self
            .sources
            .lock()
            .ok()
            .and_then(|db| db.source_count().ok())
            .unwrap_or(0);
        info!(
            existing,
            path = %path.display(),
            "sources: LinkedIn export merge import (dedupe; weekly curated refresh safe)"
        );
        let db_path = self.config.state_db_path.clone();
        let embed = Arc::clone(&self.embed);
        tokio::spawn(async move {
            match import_linkedin_export(&path, &db_path, embed.as_ref()).await {
                Ok(stats) => info!(
                    inserted = stats.inserted,
                    skipped = stats.skipped,
                    path = %path.display(),
                    "sources: LinkedIn export import done"
                ),
                Err(e) => warn!(error = %e, "sources: LinkedIn export import failed"),
            }
        });
    }

    fn try_import_linkedin_portability(&self) {
        let Some(token) = resolve_linkedin_access_token() else {
            debug!(
                "sources: no LINKEDIN_ACCESS_TOKEN yet (Member Portability OAuth into .linkedin when ready)"
            );
            return;
        };
        let db_path = self.config.state_db_path.clone();
        let embed = Arc::clone(&self.embed);
        tokio::spawn(async move {
            let client = HttpPortabilityClient::new(token);
            match import_portability_corpus(&client, &db_path, embed.as_ref()).await {
                Ok(n) => info!(count = n, "sources: imported LinkedIn portability items"),
                Err(e) => warn!(error = %e, "sources: LinkedIn portability import failed"),
            }
        });
    }

    /// Same operator path as a human `#itcy` message, without Slack Socket Mode or chat.postMessage.
    ///
    /// Used by `POST /e2e/message` (localhost). Bot self-posts are ignored by Socket Mode
    /// (`bot_id` filter in `events.rs`), so agents must inject here instead of messaging as the bot.
    pub async fn handle_operator_text(&self, text: &str) -> String {
        let preview: String = text.chars().take(240).collect();
        crate::sources::rag::log_pipeline_banner("E2E inject");
        info!(text = %preview, "e2e: injected operator message");

        match classify_channel_text(text) {
            ChannelTextKind::TextCommand(cmd) => {
                return self.dispatch_command(cmd).await;
            }
            ChannelTextKind::InlineSlash(Ok(cmd)) => {
                let outcome = self.run_slash_command(cmd).await;
                return match outcome.ack {
                    Some(ack) => format!("{ack}\n\n{}", outcome.reply),
                    None => outcome.reply,
                };
            }
            ChannelTextKind::InlineSlash(Err(usage)) => return usage,
            ChannelTextKind::Freeform => {}
        }

        let channel_id = self.config.channel_id.clone();
        let reply = self.handle_freeform(&channel_id, text).await;
        if let Err(e) = self.store_turn(&channel_id, "user", text) {
            warn!(error = %e, "e2e memory user append failed");
        }
        if let Err(e) = self.store_turn(&channel_id, "assistant", &reply) {
            warn!(error = %e, "e2e memory assistant append failed");
        }
        reply
    }

    /// Inject a slash command (localhost `POST /entrypoint/slash` only). Logs as inject, not Slack.
    pub async fn handle_operator_slash(&self, command: &str, text: &str) -> SlashCommandOutcome {
        crate::sources::rag::log_pipeline_banner("inject slash");
        info!(command = %command, text = %text.chars().take(200).collect::<String>(), "inject: slash command");
        match parse_slash_command(command, text) {
            Ok(cmd) => self.run_slash_command(cmd).await,
            Err(e) => SlashCommandOutcome {
                ack: None,
                reply: e,
            },
        }
    }

    async fn run_slash_command(&self, cmd: OperatorCommand) -> SlashCommandOutcome {
        let name = slash_command_name(&cmd);
        let ack = slash_immediate_ack(&cmd);
        let reply = self.dispatch_command(cmd).await;
        info!(
            command = %name,
            reply_chars = reply.chars().count(),
            reply_headline = %slash_reply_headline(&reply),
            "inject: slash command done"
        );
        SlashCommandOutcome { ack, reply }
    }

    /// Real Socket Mode slash: post ack **before** long work, then final reply.
    async fn handle_slash_in_channel(&self, channel_id: &str, command: &str, text: &str) {
        crate::sources::rag::log_pipeline_banner("Slack slash");
        info!(
            command = %command,
            text = %text.chars().take(200).collect::<String>(),
            "slack: slash command"
        );
        let cmd = match parse_slash_command(command, text) {
            Ok(c) => c,
            Err(e) => {
                if let Err(post_err) = post_message(&self.config.bot_token, channel_id, &e).await {
                    error!(error = %post_err, "slack slash usage post failed");
                }
                info!(command = %command, "slack: slash command done (usage error)");
                return;
            }
        };
        self.run_parsed_slash_in_channel(channel_id, cmd).await;
    }

    /// Ack + dispatch + final reply for an already-parsed operator command.
    async fn run_parsed_slash_in_channel(&self, channel_id: &str, cmd: OperatorCommand) {
        let name = slash_command_name(&cmd);
        if let Some(ack) = slash_immediate_ack(&cmd) {
            if let Err(e) = post_message(&self.config.bot_token, channel_id, &ack).await {
                error!(error = %e, "slack slash ack post failed");
            }
        }
        if let Some(batch_result) = self.try_propose_batch(&cmd).await {
            match batch_result {
                Ok(batch) => {
                    if let Err(e) = post_propose_batch(
                        &self.config.bot_token,
                        channel_id,
                        &batch.header,
                        &batch.items,
                    )
                    .await
                    {
                        error!(error = %e, "slack propose batch thread post failed");
                        if let Err(post_err) =
                            post_message(&self.config.bot_token, channel_id, &batch.summary).await
                        {
                            error!(error = %post_err, "slack propose batch fallback post failed");
                        }
                    }
                    info!(
                        command = %name,
                        items = batch.items.len(),
                        "slack: propose batch posted in thread"
                    );
                    return;
                }
                Err(e) => {
                    if let Err(post_err) =
                        post_message(&self.config.bot_token, channel_id, &e).await
                    {
                        error!(error = %post_err, "slack propose batch error post failed");
                    }
                    return;
                }
            }
        }
        let reply = self.dispatch_command(cmd).await;
        if !reply.trim().is_empty() {
            if let Err(e) = post_message(&self.config.bot_token, channel_id, &reply).await {
                error!(error = %e, "slack slash post failed");
            }
        }
        info!(
            command = %name,
            reply_chars = reply.chars().count(),
            reply_headline = %slash_reply_headline(&reply),
            "slack: slash command done"
        );
    }

    /// Handles one parsed, allowlisted event.
    pub async fn handle_event(&self, event: ParsedEvent) {
        match event {
            ParsedEvent::MemberJoined {
                channel_id,
                user_id,
            } => {
                let text = welcome_text(&user_id);
                if let Err(e) = post_message(&self.config.bot_token, &channel_id, &text).await {
                    error!(error = %e, "slack welcome post failed");
                }
            }
            ParsedEvent::SlashCommand {
                channel_id,
                user_id: _,
                command,
                text,
            } => {
                self.handle_slash_in_channel(&channel_id, &command, &text)
                    .await;
            }
            ParsedEvent::Message {
                channel_id,
                user_id: _,
                text,
            } => {
                log_slack_incoming(&text);
                match classify_channel_text(&text) {
                    ChannelTextKind::TextCommand(cmd) => {
                        let reply = self.dispatch_command(cmd).await;
                        if let Err(e) =
                            post_message(&self.config.bot_token, &channel_id, &reply).await
                        {
                            error!(error = %e, "slack command post failed");
                        }
                        return;
                    }
                    ChannelTextKind::InlineSlash(Ok(cmd)) => {
                        crate::sources::rag::log_pipeline_banner("Slack inline slash");
                        info!(
                            command = %slash_command_name(&cmd),
                            "slack: inline slash from channel text"
                        );
                        self.run_parsed_slash_in_channel(&channel_id, cmd).await;
                        return;
                    }
                    ChannelTextKind::InlineSlash(Err(usage)) => {
                        if let Err(e) =
                            post_message(&self.config.bot_token, &channel_id, &usage).await
                        {
                            error!(error = %e, "slack inline slash usage post failed");
                        }
                        return;
                    }
                    ChannelTextKind::Freeform => {}
                }

                let reply = self.handle_freeform(&channel_id, &text).await;
                if let Err(e) = self.store_turn(&channel_id, "user", &text) {
                    warn!(error = %e, "slack memory user append failed");
                }
                if let Err(e) = post_message(&self.config.bot_token, &channel_id, &reply).await {
                    error!(error = %e, "slack chat post failed");
                    return;
                }
                if let Err(e) = self.store_turn(&channel_id, "assistant", &reply) {
                    warn!(error = %e, "slack memory assistant append failed");
                }
            }
        }
    }

    async fn handle_freeform(&self, channel_id: &str, text: &str) -> String {
        if let Some(reply) = self.draft_status_short_circuit(text) {
            return reply;
        }
        let enriched = self.enrich_freeform_with_draft_status(text);
        let history = self.load_history(channel_id);
        match build_runtime_reply(&self.llm, &enriched, &history, Some(self.tools.as_ref())).await {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, "slack llm reply failed");
                llm_unavailable_reply(&e)
            }
        }
    }

    /// Ground-truth reply for "where is DRAFT-…" without asking the model.
    fn draft_status_short_circuit(&self, text: &str) -> Option<String> {
        let id = find_draft_id_in_text(text)?;
        if !looks_like_draft_status_ask(text) {
            return None;
        }
        let store = DraftStore::open(&self.config.state_db_path).ok()?;
        Some(store.get(&id).ok()?.map_or_else(
            || format!("`{id}` is **missing** in runtime.db (no draft row)."),
            |d| operator_draft_status_reply(&d),
        ))
    }

    /// Inject authoritative status when a Draft ID appears in freeform.
    fn enrich_freeform_with_draft_status(&self, text: &str) -> String {
        let Some(id) = find_draft_id_in_text(text) else {
            return text.to_string();
        };
        let Ok(store) = DraftStore::open(&self.config.state_db_path) else {
            return text.to_string();
        };
        let fact = match store.get(&id) {
            Ok(Some(d)) => crate::tools::format_stored_draft_status(&d),
            Ok(None) => format!("draft_id={id}\nstatus=missing\nNo row in runtime.db."),
            Err(_) => return text.to_string(),
        };
        format!("{text}\n\n[runtime draft_status: authoritative, do not contradict]\n{fact}")
    }

    async fn dispatch_command(&self, cmd: OperatorCommand) -> String {
        match cmd {
            OperatorCommand::Help => help_text().to_string(),
            OperatorCommand::Status => self.status_reply(),
            OperatorCommand::DraftAbout {
                subject,
                instructions,
            } => self.draft_reply(&subject, &instructions, None).await,
            OperatorCommand::DraftAboutItc {
                subject,
                instructions,
            } => self.draft_about_itc_reply(&subject, &instructions).await,
            OperatorCommand::Accept { draft_id } => {
                if draft_id.starts_with("TWEET-") {
                    self.accept_tweet_reply(&draft_id).await
                } else {
                    self.accept_draft_reply(&draft_id).await
                }
            }
            OperatorCommand::RetryBat { draft_id } => self.retry_bat_reply(&draft_id).await,
            OperatorCommand::Rework {
                draft_id,
                instructions,
            } => {
                if draft_id.starts_with("TWEET-") {
                    self.rework_tweet_reply(&draft_id, &instructions).await
                } else {
                    self.rework_draft_reply(&draft_id, &instructions).await
                }
            }
            OperatorCommand::ChangeUrl { draft_id, choice } => {
                if draft_id.starts_with("TWEET-") {
                    self.change_tweet_url_reply(&draft_id, &choice)
                } else {
                    self.change_draft_url_reply(&draft_id, &choice)
                }
            }
            OperatorCommand::List => self.list_saved_reply(),
            OperatorCommand::Show { ids } => self.show_saved_ids_reply(&ids).await,
            OperatorCommand::Delete { ids } => self.delete_saved_ids_reply(&ids).await,
            OperatorCommand::ProposeDraft { digest_id, indices } => {
                if digest_id.is_none() && indices.is_empty() {
                    self.draft_reply(
                        "what we know",
                        "Propose one Interchouette ITC company-page post from corpus memory (voice and history). Pick the strongest current subject. This is not a daily-digest pick.",
                        None,
                    )
                    .await
                } else {
                    self.propose_draft_reply(digest_id.as_deref(), &indices)
                        .await
                }
            }
            OperatorCommand::DailyDigest => self.daily_digest_reply().await,
            OperatorCommand::AcceptCommentReply { url } => {
                self.accept_comment_reply_reply(&url).await
            }
            OperatorCommand::ShipCommentReply { url } => self.ship_comment_reply_reply(&url).await,
            OperatorCommand::Enrich { url } => self.enrich_reply(&url).await,
            OperatorCommand::Ingest { url } => self.ingest_reply(&url).await,
            OperatorCommand::HandleAdd { raw } => self.handle_add_reply(&raw),
            OperatorCommand::TweetAbout {
                subject,
                instructions,
            } => self.tweet_reply(&subject, &instructions).await,
            OperatorCommand::TweetFarce { theme } => self.tweet_farce_reply(&theme).await,
            OperatorCommand::DraftTweetAboutItc {
                subject,
                instructions,
            } => {
                self.draft_tweet_about_itc_reply(&subject, &instructions)
                    .await
            }
            OperatorCommand::DraftAboutItcy { instructions } => {
                self.draft_about_itcy_reply(&instructions).await
            }
            OperatorCommand::TweetAboutItcy { instructions } => {
                self.tweet_about_itcy_reply(&instructions).await
            }
            OperatorCommand::ProposeTweet { digest_id, indices } => {
                if digest_id.is_none() && indices.is_empty() {
                    self.tweet_reply(
                        "what we know",
                        "Propose one Interchouette ITC X tweet from corpus memory (voice and history). Pick the strongest current subject. This is not a daily-digest pick.",
                    )
                    .await
                } else {
                    self.propose_tweet_reply(digest_id.as_deref(), &indices)
                        .await
                }
            }
        }
    }

    async fn daily_digest_reply(&self) -> String {
        let db = self.config.state_db_path.clone();
        let rec = match build_daily_digest(&db).await {
            Ok(r) => r,
            Err(e) => return format!("`/daily_digest` failed: {e}"),
        };
        let digest_ch = self.config.daily_digest_channel_id.trim();
        if digest_ch.is_empty() {
            return format!(
                "{text}\n\n_(Set SLACK_DAILY_DIGEST_CHANNEL_ID to post this into #daily-digest; replied here only.)_",
                text = format_digest_slack(&rec)
            );
        }
        let post = digest_slack_post(&rec);
        match post_digest_channel(&self.config.bot_token, digest_ch, &post).await {
            Ok(()) => String::new(),
            Err(e) => format!(
                "Digest `{id}` stored but Slack post to #daily-digest failed: {e}\n\nRe-post with `/show {id}`.",
                id = rec.digest_id,
            ),
        }
    }

    async fn propose_draft_reply(&self, digest_id: Option<&str>, indices: &[i32]) -> String {
        match self.propose_draft_batch(digest_id, indices).await {
            Ok(batch) => batch.summary,
            Err(e) => e,
        }
    }

    async fn propose_draft_batch(
        &self,
        digest_id: Option<&str>,
        indices: &[i32],
    ) -> Result<ProposeBatch, String> {
        let db = self.config.state_db_path.as_path();
        let (rec, picked) =
            crate::sources::digest::load_digest_pick(db, digest_id, indices, "/propose_draft")?;
        let header = format!(
            "From `{digest}`: starting {n} draft(s):",
            digest = rec.digest_id,
            n = picked.len()
        );
        let mut items = Vec::with_capacity(picked.len());
        for it in &picked {
            let (subject, instructions) = crate::sources::digest::digest_propose_brief(it);
            let cite = it.url.as_deref().filter(|u| !u.trim().is_empty());
            let reply = self.draft_reply(&subject, &instructions, cite).await;
            items.push(format!("--- item {} ---\n{reply}", it.idx));
        }
        Ok(ProposeBatch::new(header, items))
    }

    async fn try_propose_batch(
        &self,
        cmd: &OperatorCommand,
    ) -> Option<Result<ProposeBatch, String>> {
        match cmd {
            OperatorCommand::ProposeDraft { digest_id, indices }
                if digest_id.is_some() || !indices.is_empty() =>
            {
                Some(
                    self.propose_draft_batch(digest_id.as_deref(), indices)
                        .await,
                )
            }
            OperatorCommand::ProposeTweet { digest_id, indices }
                if digest_id.is_some() || !indices.is_empty() =>
            {
                Some(
                    self.propose_tweet_batch(digest_id.as_deref(), indices)
                        .await,
                )
            }
            _ => None,
        }
    }

    async fn accept_comment_reply_reply(&self, url: &str) -> String {
        match crate::sources::linkedin_comment::draft_comment_reply_for_slack(&self.llm, url).await
        {
            Ok(msg) => msg,
            Err(e) => format!("`/accept_comment_reply` failed: {e}"),
        }
    }

    async fn ship_comment_reply_reply(&self, url: &str) -> String {
        match crate::sources::linkedin_comment::ship_comment_reply_via_mcp(&self.llm, url).await {
            Ok(msg) => msg,
            Err(e) => format!("`/ship_comment_reply` failed: {e}"),
        }
    }

    async fn enrich_reply(&self, url: &str) -> String {
        let url = url.to_string();
        let db_path = self.config.state_db_path.clone();
        let embed = Arc::clone(&self.embed);
        tokio::task::spawn_blocking(move || manual_enrich_slack_reply(&url, &db_path, &embed))
            .await
            .unwrap_or_else(|_| "Internal: enrich worker failed".into())
    }

    async fn ingest_reply(&self, url: &str) -> String {
        let fetcher = HttpThenPublicPlaywright::new();
        match ingest_url(
            url,
            &self.config.state_db_path,
            self.embed.as_ref(),
            &fetcher,
        )
        .await
        {
            Ok(report) => {
                info!(
                    source_id = report.source_id,
                    subject = %report.subject,
                    chars = report.chars,
                    chunks = report.chunks,
                    fetch = report.fetch_path.as_str(),
                    "slack: /ingest done"
                );
                report.slack_message()
            }
            Err(e) => format!("`/ingest` failed for `{url}`: {e}"),
        }
    }

    fn handle_add_reply(&self, raw: &str) -> String {
        match self.tools.handle_add(raw) {
            Ok(outcome) => crate::sources::handles::format_handle_add_reply(&outcome),
            Err(e) => format!("`/handle_add` failed: {e}"),
        }
    }

    async fn draft_reply(&self, topic: &str, instructions: &str, cite_url: Option<&str>) -> String {
        let operator_brief = compose_operator_brief(topic, instructions);
        let draft_id = crate::sources::draft_footer::next_draft_id(&self.config.state_db_path)
            .unwrap_or_else(|e| {
                warn!(error = %e, "slack: draft id allocate failed; using fallback");
                format!("DRAFT-{}-UNKNOWN", chrono::Local::now().format("%Y%m%d"))
            });
        if let Err(e) = DraftStore::open(&self.config.state_db_path)
            .and_then(|s| s.upsert(&stored_building_stub(&draft_id, topic.trim())))
        {
            warn!(error = %e, draft_id = %draft_id, "slack: building stub persist failed");
        }
        match self
            .tools
            .begin_research_session(&operator_brief, &draft_id)
            .await
        {
            Ok(dir) => {
                crate::logging::append_session_log_note(&format!(
                    "slack: /draft_about\ndraft_id: {draft_id}\ntopic: {topic}\ninstructions: {instructions}\nsession: {}\n",
                    dir.display()
                ));
                info!(
                    dir = %dir.display(),
                    draft_id = %draft_id,
                    "slack: draft research session"
                );
            }
            Err(e) => warn!(error = %e, "slack: could not start research session log"),
        }
        match build_grounded_draft_with_cite(
            &self.llm,
            &self.config.state_db_path,
            self.embed.as_ref(),
            &operator_brief,
            Some(self.tools.as_ref()),
            cite_url,
        )
        .await
        {
            Ok(mut draft) => {
                draft.subject = topic.trim().to_string();
                if let Err(e) = self.persist_grounded_draft(&draft) {
                    error!(error = %e, "slack draft store failed");
                }
                format!(
                    "{body}\n\n\
:floppy_disk: Saved as open draft. Ref `{id}`.\n\n\
{next}",
                    body = slack_paste_safe_linkedin_message(&draft.body),
                    id = draft.draft_id,
                    next = crate::slack::saved::next_slash_hints(&draft.draft_id, status::OPEN)
                )
            }
            Err(RagError::NoSources(s)) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&draft_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                format!(
                    "No corpus hits yet for `{s}`, and the model could not ground a draft.\n\n\
Try /draft_about again, or paste a public article URL into freeform so ingest can save it.\n\n\
Draft `{draft_id}` marked failed"
                )
            }
            Err(e) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&draft_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                error!(error = %e, "slack grounded draft failed");
                format!("ITCy could not build a grounded draft ({e}). Draft `{draft_id}` marked failed.")
            }
        }
    }

    async fn draft_about_itc_reply(&self, topic: &str, instructions: &str) -> String {
        use crate::sources::itc_digest::{build_itc_research_pack, default_itc_subject};
        use crate::sources::rag::build_grounded_draft_from_pack;

        let subject = if topic.trim().is_empty() {
            match default_itc_subject().await {
                Ok(s) => s,
                Err(e) => return format!("`/draft_about_itc` failed: {e}"),
            }
        } else {
            topic.trim().to_string()
        };
        let operator_brief = compose_operator_brief(&subject, instructions);
        let (pack, urls) = match build_itc_research_pack(&operator_brief).await {
            Ok(v) => v,
            Err(e) => return format!("`/draft_about_itc` failed: {e}"),
        };

        let draft_id = crate::sources::draft_footer::next_draft_id(&self.config.state_db_path)
            .unwrap_or_else(|e| {
                warn!(error = %e, "slack: draft id allocate failed; using fallback");
                format!("DRAFT-{}-UNKNOWN", chrono::Local::now().format("%Y%m%d"))
            });
        if let Err(e) = DraftStore::open(&self.config.state_db_path)
            .and_then(|s| s.upsert(&stored_building_stub(&draft_id, subject.trim())))
        {
            warn!(error = %e, draft_id = %draft_id, "slack: building stub persist failed");
        }
        match self
            .tools
            .begin_research_session(&operator_brief, &draft_id)
            .await
        {
            Ok(dir) => {
                crate::logging::append_session_log_note(&format!(
                    "slack: /draft_about_itc\ndraft_id: {draft_id}\ntopic: {subject}\ninstructions: {instructions}\nsession: {}\n",
                    dir.display()
                ));
                info!(
                    dir = %dir.display(),
                    draft_id = %draft_id,
                    "slack: itc draft research session"
                );
            }
            Err(e) => warn!(error = %e, "slack: could not start research session log"),
        }
        match build_grounded_draft_from_pack(
            &self.llm,
            &self.config.state_db_path,
            &operator_brief,
            &pack,
            &urls,
            Some(self.tools.as_ref()),
        )
        .await
        {
            Ok(mut draft) => {
                draft.subject = subject;
                if let Err(e) = self.persist_grounded_draft(&draft) {
                    error!(error = %e, "slack draft store failed");
                }
                format!(
                    "{body}\n\n\
:floppy_disk: Saved as open draft. Ref `{id}`.\n\n\
{next}",
                    body = slack_paste_safe_linkedin_message(&draft.body),
                    id = draft.draft_id,
                    next = crate::slack::saved::next_slash_hints(&draft.draft_id, status::OPEN)
                )
            }
            Err(e) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&draft_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                error!(error = %e, "slack itc grounded draft failed");
                format!(
                    "ITCy could not build an Interchouette draft ({e}). Draft `{draft_id}` marked failed."
                )
            }
        }
    }

    async fn draft_about_itcy_reply(&self, instructions: &str) -> String {
        use crate::sources::self_intro::build_itcy_self_draft;

        let subject = "ITCy self-introduction";
        let draft_id = crate::sources::draft_footer::next_draft_id(&self.config.state_db_path)
            .unwrap_or_else(|e| {
                warn!(error = %e, "slack: draft id allocate failed; using fallback");
                format!("DRAFT-{}-UNKNOWN", chrono::Local::now().format("%Y%m%d"))
            });
        if let Err(e) = DraftStore::open(&self.config.state_db_path)
            .and_then(|s| s.upsert(&stored_building_stub(&draft_id, subject)))
        {
            warn!(error = %e, draft_id = %draft_id, "slack: self-intro building stub persist failed");
        }
        match self.tools.begin_research_session(subject, &draft_id).await {
            Ok(dir) => {
                crate::logging::append_session_log_note(&format!(
                    "slack: /draft_about_itcy\ndraft_id: {draft_id}\ninstructions: {instructions}\nsession: {}\n",
                    dir.display()
                ));
                info!(
                    dir = %dir.display(),
                    draft_id = %draft_id,
                    "slack: itcy self-intro draft session"
                );
            }
            Err(e) => warn!(error = %e, "slack: could not start self-intro session log"),
        }
        match build_itcy_self_draft(
            &self.llm,
            &self.config.state_db_path,
            instructions,
            Some(self.tools.as_ref()),
        )
        .await
        {
            Ok(mut draft) => {
                draft.subject = subject.to_string();
                if let Err(e) = self.persist_grounded_draft(&draft) {
                    error!(error = %e, "slack self-intro draft store failed");
                }
                format!(
                    "{body}\n\n\
:floppy_disk: Saved as open draft. Ref `{id}`.\n\n\
{next}",
                    body = slack_paste_safe_linkedin_message(&draft.body),
                    id = draft.draft_id,
                    next = crate::slack::saved::next_slash_hints(&draft.draft_id, status::OPEN)
                )
            }
            Err(e) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&draft_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                error!(error = %e, "slack itcy self-intro draft failed");
                format!(
                    "ITCy could not build a self-introduction draft ({e}). Draft `{draft_id}` marked failed."
                )
            }
        }
    }

    async fn accept_draft_reply(&self, draft_id: &str) -> String {
        if draft_id.starts_with("TWEET-") {
            return "use `/accept` with a TWEET- id".into();
        }
        match accept_draft(&self.config.state_db_path, draft_id).await {
            Ok(r) => {
                if let Some(p) = r.promoted {
                    return format!(
                        "Approve was already on GitHub (webhook had been missed): **Post published**:\n\
• draft: `{draft}`\n\
• post: `{post}`\n\
• fork PR: #{pr} → merged\n\
• {detail}\n\
Status: **published**.",
                        draft = p.draft_id,
                        post = p.post_id,
                        pr = p.pr_number,
                        detail = p.detail
                    );
                }
                let action = if r.updated_existing {
                    "Draft PR **updated** (same fork PR; status was already accepted: ok to re-run)"
                } else {
                    "Draft PR **opened** (fork)"
                };
                let paste = paste_block_for_draft(&self.config.state_db_path, &r.draft_id);
                let next = crate::slack::saved::next_slash_hints(&r.draft_id, status::ACCEPTED);
                format!(
                    ":white_check_mark: {action}:\n\
• draft: `{id}`\n\
• branch: `{branch}`\n\
• PR: {url}\n\
:hourglass_flowing_sand: Status: **accepted**. Waiting **gRoussac** Approve = BAT → Post on Interchouette (playground soft ship).\n\n\
{paste}\n\n\
{next}",
                    id = r.draft_id,
                    branch = r.branch,
                    url = r.pr_url,
                    paste = paste,
                    next = next,
                )
            }
            Err(e) => format!("Could not accept Draft: {e}"),
        }
    }

    async fn retry_bat_reply(&self, draft_id: &str) -> String {
        match retry_bat(&self.config.state_db_path, draft_id).await {
            Ok(r) => {
                let is_tweet = r.draft_id.starts_with("TWEET-") || r.post_id.starts_with("XPOST-");
                if is_tweet {
                    format!(
                        "Re-shipped after BAT:\n\
• tweet: `{draft}`\n\
• xpost: `{post}`\n\
• {detail}\n\
Status: **published** (do not rework this id).",
                        draft = r.draft_id,
                        post = r.post_id,
                        detail = r.detail
                    )
                } else {
                    format!(
                        "Re-shipped after BAT:\n\
• draft: `{draft}`\n\
• post: `{post}`\n\
• {detail}\n\
Status: **published** (do not rework this id).",
                        draft = r.draft_id,
                        post = r.post_id,
                        detail = r.detail
                    )
                }
            }
            Err(e) => format!("Could not ship after BAT: {e}\n\n/retry_bat {draft_id}"),
        }
    }

    async fn rework_draft_reply(&self, draft_id: &str, instructions: &str) -> String {
        let stored = match ensure_open_for_edit(&self.config.state_db_path, draft_id) {
            Ok(d) => d,
            Err(e) => return format!("Could not edit draft: {e}"),
        };
        let store = match DraftStore::open(&self.config.state_db_path) {
            Ok(s) => s,
            Err(e) => return format!("Could not open draft store: {e}"),
        };
        match rework_stored_draft(&self.llm, &stored, instructions, Some(self.tools.as_ref())).await
        {
            Ok(rew) => {
                let mut row = stored_from_payload(DraftPayload {
                    draft_id: rew.draft_id.clone(),
                    subject: rew.subject.clone(),
                    body: rew.body.clone(),
                    model: rew.model.clone(),
                    tokens_in: rew.tokens_in,
                    tokens_out: rew.tokens_out,
                    sources: rew.sources,
                    link_options: rew.link_options,
                    research_pack: rew.research_pack,
                });
                row.fork_pr_number = stored.fork_pr_number;
                row.fork_pr_url.clone_from(&stored.fork_pr_url);
                row.created_at.clone_from(&stored.created_at);
                if let Err(e) = store.upsert(&row) {
                    return format!(
                        "Rework produced a body but store failed: {e}\n\n{}",
                        rew.body
                    );
                }
                format!(
                    "{body}\n\n\
:arrows_counterclockwise: Reworked draft `{id}` saved (**open**).\n\n\
{next}",
                    body = slack_paste_safe_linkedin_message(&rew.body),
                    id = rew.draft_id,
                    next = crate::slack::saved::next_slash_hints(&rew.draft_id, status::OPEN)
                )
            }
            Err(e) => format!("Could not rework draft: {e}"),
        }
    }

    fn change_draft_url_reply(&self, draft_id: &str, choice: &str) -> String {
        let mut stored = match ensure_open_for_edit(&self.config.state_db_path, draft_id) {
            Ok(d) => d,
            Err(e) => return format!("Could not edit draft: {e}"),
        };
        let store = match DraftStore::open(&self.config.state_db_path) {
            Ok(s) => s,
            Err(e) => return format!("Could not open draft store: {e}"),
        };
        let picked = match resolve_url_choice(choice, &stored.link_options) {
            Ok(u) => u,
            Err(e) => return e,
        };
        let prior = extract_in_post_url(&stored.body);
        let mut before_options = stored.body.clone();
        if let Some(i) = footer_start(&before_options) {
            before_options = before_options[..i].to_string();
        }
        if before_options.starts_with("Draft ID:") {
            if let Some((_, rest)) = before_options.split_once('\n') {
                before_options = rest.trim_start().to_string();
            }
        }
        match picked {
            UrlChoice::Clear => {
                info!(
                    draft_id = %draft_id,
                    from = ?prior,
                    "change_draft_url: clearing link"
                );
                before_options = set_single_in_post_url(before_options.trim(), "");
                stored.body = crate::llm::disclosure::ensure_stored_disclosure(
                    &compose_draft_message(before_options.trim(), draft_id, &stored.link_options),
                    &stored.model,
                    stored.tokens_in,
                    stored.tokens_out,
                );
                stored.status = status::OPEN.into();
                stored.updated_at = chrono::Local::now().to_rfc3339();
                if let Err(e) = store.upsert(&stored) {
                    return format!("Link clear failed to save: {e}");
                }
                format!(
                    "{body}\n\n\
:link: Link cleared.\n\n\
{next}",
                    body = slack_paste_safe_linkedin_message(&stored.body),
                    next = crate::slack::saved::next_slash_hints(draft_id, status::OPEN),
                )
            }
            UrlChoice::Url(new_url) => {
                info!(
                    draft_id = %draft_id,
                    choice = %choice,
                    from = ?prior,
                    to = %new_url,
                    "change_draft_url: swapping link"
                );
                before_options = set_single_in_post_url(before_options.trim(), &new_url);
                promote_link_option(&mut stored.link_options, &new_url);
                stored.body = crate::llm::disclosure::ensure_stored_disclosure(
                    &compose_draft_message(before_options.trim(), draft_id, &stored.link_options),
                    &stored.model,
                    stored.tokens_in,
                    stored.tokens_out,
                );
                stored.status = status::OPEN.into();
                stored.updated_at = chrono::Local::now().to_rfc3339();
                if let Err(e) = store.upsert(&stored) {
                    return format!("Link swap failed to save: {e}");
                }
                info!(
                    draft_id = %draft_id,
                    in_post = %new_url,
                    option1 = ?stored.link_options.first(),
                    "change_draft_url: saved"
                );
                format!(
                    "{body}\n\n\
:link: Link updated.\n\n\
{next}",
                    body = slack_paste_safe_linkedin_message(&stored.body),
                    next = crate::slack::saved::next_slash_hints(draft_id, status::OPEN),
                )
            }
        }
    }

    pub(crate) fn persist_grounded_draft(
        &self,
        draft: &crate::sources::GroundedDraft,
    ) -> Result<(), String> {
        let store = DraftStore::open(&self.config.state_db_path).map_err(|e| e.to_string())?;
        let prior = store.get(&draft.draft_id).map_err(|e| e.to_string())?;
        let mut row = stored_from_payload(DraftPayload {
            draft_id: draft.draft_id.clone(),
            subject: draft.subject.clone(),
            body: draft.body.clone(),
            model: draft.model.clone(),
            tokens_in: draft.tokens_in,
            tokens_out: draft.tokens_out,
            sources: draft.source_labels.clone(),
            link_options: draft.link_options.clone(),
            research_pack: draft.research_pack.clone(),
        });
        if let Some(p) = prior {
            row.created_at = p.created_at;
            row.fork_pr_number = p.fork_pr_number;
            row.fork_pr_url = p.fork_pr_url;
        }
        store.upsert(&row).map_err(|e| e.to_string())
    }

    fn status_reply(&self) -> String {
        let recent = self.load_history(&self.config.channel_id);
        let connected = self.slack_connected.lock().is_ok_and(|g| *g);
        let providers = self.llm.provider_ids().join(", ");
        let freeform_route_head = self.llm.route_head_label(TaskKind::Freeform);
        let freeform_route = self.llm.route(TaskKind::Freeform);
        let load_route_head = self.llm.route_head_label(TaskKind::Load);
        let load_route = self.llm.route(TaskKind::Load);
        let draft_route_head = self.llm.route_head_label(TaskKind::Draft);
        let draft_route = self.llm.route(TaskKind::Draft);
        let source_count = self
            .sources
            .lock()
            .ok()
            .and_then(|db| db.source_count().ok())
            .unwrap_or(0);
        let linkedin_publish_mode = crate::publish::resolve_publish_mode_agile("playground")
            .map_or_else(|_| "playground".into(), |m| m.as_str().to_string());
        let x_publish_mode = crate::publish::resolve_x_publish_mode("playground")
            .map_or_else(|_| "playground".into(), |m| m.as_str().to_string());
        let linkedin_mcp = crate::publish::probe_linkedin_mcp();
        status_text(&StatusSnapshot {
            bind: &self.config.bind,
            channel_id: &self.config.channel_id,
            slack_connected: connected,
            max_context_messages: self.config.max_context_messages,
            recent: &recent,
            providers: &providers,
            freeform_route_head: &freeform_route_head,
            freeform_route: &freeform_route,
            load_route_head: &load_route_head,
            load_route: &load_route,
            draft_route_head: &draft_route_head,
            draft_route: &draft_route,
            source_count,
            linkedin_publish_mode: &linkedin_publish_mode,
            x_publish_mode: &x_publish_mode,
            linkedin_mcp: &linkedin_mcp.detail,
        })
    }

    fn load_history(&self, session_id: &str) -> Vec<StoredMessage> {
        let Ok(db) = self.memory.lock() else {
            return Vec::new();
        };
        db.get_last_messages(session_id, self.config.max_context_messages)
            .unwrap_or_default()
    }

    fn store_turn(&self, session_id: &str, role: &str, content: &str) -> Result<(), String> {
        let db = self
            .memory
            .lock()
            .map_err(|_| "memory lock poisoned".to_string())?;
        db.append_message(session_id, role, content)
            .map_err(|e| e.to_string())
    }
}

/// True when the event targets the configured `#itcy` channel.
#[must_use]
pub fn event_allowed(runtime: &SlackRuntime, event: &ParsedEvent) -> bool {
    let channel_id = match event {
        ParsedEvent::Message { channel_id, .. }
        | ParsedEvent::MemberJoined { channel_id, .. }
        | ParsedEvent::SlashCommand { channel_id, .. } => channel_id.as_str(),
    };
    is_channel_allowed(&runtime.config.channel_id, channel_id)
}

/// Reads Slack env vars named in config. Returns `None` when incomplete (health still runs).
pub fn resolve_slack_runtime(config: &Config) -> Option<SlackRuntimeConfig> {
    let slack = &config.slack;
    if slack.events_transport != "socket" {
        warn!(
            transport = %slack.events_transport,
            "slack: events_transport not supported yet (need socket); skipping Slack runtime"
        );
        return None;
    }

    let bot_token = std::env::var(&slack.bot_token_env)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let app_token = std::env::var(&slack.app_token_env)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let channel_id = std::env::var(&slack.channel_env)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let daily_digest_channel_id = std::env::var(&slack.daily_digest_channel_env)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();

    let mut missing = Vec::new();
    if bot_token.is_none() {
        missing.push(slack.bot_token_env.as_str());
    }
    if app_token.is_none() {
        missing.push(slack.app_token_env.as_str());
    }
    if channel_id.is_none() {
        missing.push(slack.channel_env.as_str());
    }
    let (Some(bot_token), Some(app_token), Some(channel_id)) = (bot_token, app_token, channel_id)
    else {
        warn!(
            missing = %missing.join(", "),
            "slack: unset env; /health only (set them in `.env`)"
        );
        return None;
    };
    if daily_digest_channel_id.is_empty() {
        info!(
            env = %slack.daily_digest_channel_env,
            "slack: daily digest channel unset; /daily_digest stores locally until set"
        );
    }

    Some(SlackRuntimeConfig {
        bot_token,
        app_token,
        channel_id,
        daily_digest_channel_id,
        bind: config.server.bind.clone(),
        max_context_messages: config.runtime.max_context_messages,
        state_db_path: PathBuf::from(&config.runtime.state_db_path),
        linkedin_export_dir: resolve_export_path(&config.runtime.linkedin_export_dir),
    })
}

fn manual_enrich_slack_reply(
    url: &str,
    db_path: &std::path::Path,
    embed: &Arc<dyn EmbedClient>,
) -> String {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("enrich runtime");
    rt.block_on(manual_enrich_slack_reply_async(
        url,
        db_path,
        embed.as_ref(),
    ))
}

fn enrich_display_title(stored_title: &str, raw_text: &str) -> String {
    let t = stored_title.trim();
    if !t.is_empty() && !t.eq_ignore_ascii_case("post") && !t.eq_ignore_ascii_case("repost") {
        return t.to_string();
    }
    let line = raw_text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(t);
    let cleaned = line
        .trim_start_matches(|c: char| c == '*' || c == '#' || c.is_whitespace())
        .trim();
    if cleaned.is_empty() {
        return if t.is_empty() {
            "LinkedIn post".into()
        } else {
            t.to_string()
        };
    }
    content_preview(cleaned, 100)
}

/// Opens `SourceDb` / scrape cache (`rusqlite::Connection`, !Send) across `.await`.
#[allow(clippy::future_not_send)]
async fn manual_enrich_slack_reply_async(
    url: &str,
    db_path: &std::path::Path,
    embed: &dyn EmbedClient,
) -> String {
    let socks = std::env::var("ITCY_TOR_SOCKS").unwrap_or_else(|_| DEFAULT_TOR_SOCKS.to_string());
    let control =
        std::env::var("ITCY_TOR_CONTROL").unwrap_or_else(|_| DEFAULT_TOR_CONTROL.to_string());
    let fetcher = match TorSocksFetcher::new(&socks) {
        Ok(f) => f,
        Err(e) => return format!("`/enrich` Tor client failed: {e}"),
    };
    let cache_path = resolve_scrape_cache_path("");
    let cache = match ScrapeCache::open(&cache_path) {
        Ok(c) => c,
        Err(e) => return format!("`/enrich` scrape cache open failed: {e}"),
    };
    let db = match SourceDb::open(db_path) {
        Ok(d) => d,
        Err(e) => return format!("`/enrich` source DB open failed: {e}"),
    };
    match enrich_linkedin_url_at(
        &db,
        &cache,
        &fetcher,
        embed,
        Some(&control),
        url,
    )
    .await
    {
        Ok(EnrichManualResult::Ok { source_id, via }) => {
            let via_label = match via {
                EnrichManualVia::Cache => "cache",
                EnrichManualVia::Tor => "Tor",
            };
            let detail = db
                .get_source(source_id)
                .ok()
                .flatten().map_or_else(|| {
                    format!(
                        "*Enrich complete*\n• source `#{source_id}` · via **{via_label}**\n`{url}`"
                    )
                }, |row| {
                    let preview = content_preview(&row.raw_text, 320);
                    let title = enrich_display_title(&row.title, &row.raw_text);
                    let chunks = db
                        .get_chunk_candidates(&row.subject, 50)
                        .map_or(0, |c| c.len());
                    format!(
                        "*Enrich complete*\n\
• source `#{source_id}` · via **{via_label}**\n\
• subject: `{subject}`\n\
• title: {title}\n\
• text: {chars} chars · ~{chunks} chunks · embed `nomic-embed-text` (Ollama)\n\
• preview: {preview}\n\
• same `runtime.db` as `/ingest` + draft `corpus_search`\n\
`{url}`",
                        subject = row.subject,
                        title = title,
                        chars = row.raw_text.chars().count(),
                        chunks = chunks,
                        preview = preview,
                        url = url,
                        source_id = source_id,
                        via_label = via_label,
                    )
                });
            info!(source_id, via = %via_label, "slack: /enrich done");
            detail
        }
        Ok(EnrichManualResult::Skipped {
            source_id,
            reason,
        }) => format!(
            "LinkedIn post parked (source #{source_id}, reason: {reason}). Not in drip until manual requeue.\n`{url}`"
        ),
        Ok(EnrichManualResult::Wall {
            source_id,
            after,
        }) => format!(
            "LinkedIn wall/rate limit (source #{source_id}). Backoff until {after}.\n`{url}`"
        ),
        Ok(EnrichManualResult::Failed {
            source_id,
            after,
        }) => format!(
            "Enrich failed (source #{source_id}). Retry after {after}.\n`{url}`"
        ),
        Err(e) => format!("`/enrich` failed for `{url}`: {e}"),
    }
}

/// Freeform "where is this draft" / status questions (answered from `SQLite`, not the model).
/// Includes FR operator phrasing needles (`où en est`, `état`, …) beside English.
fn looks_like_draft_status_ask(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("where")
        || lower.contains("status")
        || lower.contains("pending")
        || lower.contains("où en est")
        || lower.contains("ou en est")
        || lower.contains("état")
        || lower.contains("etat")
        || lower.contains("où sommes")
        || lower.contains("ou sommes")
}

/// Load draft row and build the Slack-fenced manual `LinkedIn` paste block.
fn paste_block_for_draft(db_path: &std::path::Path, draft_id: &str) -> String {
    let Ok(store) = DraftStore::open(db_path) else {
        return String::new();
    };
    let Ok(Some(row)) = store.get(draft_id) else {
        return String::new();
    };
    linkedin_manual_paste_message(&row.body, &row.model, row.tokens_in, row.tokens_out)
}

#[cfg(test)]
mod tests {
    use super::looks_like_draft_status_ask;
    use crate::llm::client::{LlmClient, LlmError, LlmMessage, LlmResponse, LlmRole, LlmToolDef};
    use crate::llm::router::{ChainCandidate, FailoverRouter, TaskChains, TaskKind};
    use crate::slack::commands::{
        classify_channel_text, find_draft_id_in_text, parse_slash_command, parse_text_command,
        ChannelTextKind, OperatorCommand,
    };
    use crate::slack::handler::{SlackRuntime, SlackRuntimeConfig};
    use crate::sources::embed::MockEmbedClient;
    use crate::tools::ItcyTools;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn draft_whereabouts_detected() {
        let msg = "where are we on DRAFT-20260803-000028";
        assert_eq!(
            find_draft_id_in_text(msg).as_deref(),
            Some("DRAFT-20260803-000028")
        );
        assert!(looks_like_draft_status_ask(msg));
    }

    #[test]
    fn help_and_status_bypass_llm_path() {
        assert!(parse_text_command("help").is_some());
        assert!(parse_text_command("status_itcy").is_some());
        assert!(parse_text_command("status").is_none());
        assert!(parse_text_command("tell me a joke").is_none());
    }

    #[test]
    fn slash_workflows_parse() {
        assert!(parse_slash_command("/draft_about", "rust async, find recent news").is_ok());
        assert!(
            parse_slash_command("/enrich", "https://www.linkedin.com/posts/gregoryroussac_x")
                .is_ok()
        );
        assert!(parse_slash_command("/ingest", "https://example.com/article").is_ok());
    }

    struct CountingLlm {
        calls: AtomicUsize,
        reply: String,
    }

    #[async_trait]
    impl LlmClient for CountingLlm {
        fn provider_id(&self) -> &'static str {
            "test"
        }

        async fn chat(
            &self,
            _messages: &[LlmMessage],
            _model: &str,
            _tools: Option<&[LlmToolDef]>,
        ) -> Result<LlmResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(LlmResponse {
                message: LlmMessage {
                    role: LlmRole::Assistant,
                    content: self.reply.clone(),
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: "stop".into(),
                usage: None,
            })
        }
    }

    fn test_runtime(llm: Arc<dyn LlmClient>) -> SlackRuntime {
        let dir = std::env::temp_dir().join(format!(
            "itcy-inline-slash-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("runtime.db");
        let export = dir.join("linkedin-export");
        let _ = std::fs::create_dir_all(&export);
        let embed: Arc<dyn crate::sources::embed::EmbedClient> = Arc::new(MockEmbedClient);
        let tools = Arc::new(ItcyTools::new(
            db.clone(),
            Arc::clone(&embed),
            PathBuf::from("/bin/false"),
        ));
        let mut clients = HashMap::new();
        clients.insert("test".into(), llm);
        let chains = TaskChains::new().with_chain(
            TaskKind::Freeform,
            vec![ChainCandidate::new("test", "mock")],
        );
        let router = Arc::new(FailoverRouter::new(clients, chains));
        let cfg = SlackRuntimeConfig {
            bot_token: "xoxb-test".into(),
            app_token: "xapp-test".into(),
            channel_id: "C_TEST".into(),
            daily_digest_channel_id: String::new(),
            bind: "127.0.0.1:0".into(),
            max_context_messages: 8,
            state_db_path: db,
            linkedin_export_dir: export,
        };
        SlackRuntime::new(cfg, router, embed, tools).expect("runtime")
    }

    #[tokio::test]
    async fn operator_text_unknown_slash_token_is_freeform() {
        let llm = Arc::new(CountingLlm {
            calls: AtomicUsize::new(0),
            reply: "FREEFORM_HIT".into(),
        });
        let llm_client: Arc<dyn LlmClient> = llm.clone();
        let rt = test_runtime(llm_client);
        assert_eq!(
            classify_channel_text("Use /not_a_real_itcy_cmd please"),
            ChannelTextKind::Freeform
        );
        let reply = rt
            .handle_operator_text("Use /not_a_real_itcy_cmd please")
            .await;
        assert!(reply.contains("FREEFORM_HIT"), "{reply}");
        assert_eq!(llm.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn operator_text_inline_list_skips_llm() {
        let llm = Arc::new(CountingLlm {
            calls: AtomicUsize::new(0),
            reply: "SHOULD_NOT_SEE".into(),
        });
        let llm_client: Arc<dyn LlmClient> = llm.clone();
        let rt = test_runtime(llm_client);
        assert!(matches!(
            classify_channel_text("Use /list"),
            ChannelTextKind::InlineSlash(Ok(OperatorCommand::List))
        ));
        let reply = rt.handle_operator_text("Use /list").await;
        assert!(!reply.contains("SHOULD_NOT_SEE"), "{reply}");
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn operator_text_freeform_hits_llm_once() {
        let llm = Arc::new(CountingLlm {
            calls: AtomicUsize::new(0),
            reply: "FREEFORM_HIT".into(),
        });
        let llm_client: Arc<dyn LlmClient> = llm.clone();
        let rt = test_runtime(llm_client);
        assert_eq!(
            classify_channel_text("say hello without any slash"),
            ChannelTextKind::Freeform
        );
        let reply = rt.handle_operator_text("say hello without any slash").await;
        assert!(reply.contains("FREEFORM_HIT"), "{reply}");
        assert_eq!(llm.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn operator_text_use_propose_draft_is_inline_not_freeform() {
        let llm = Arc::new(CountingLlm {
            calls: AtomicUsize::new(0),
            reply: "FREEFORM_HIT".into(),
        });
        let llm_client: Arc<dyn LlmClient> = llm.clone();
        let rt = test_runtime(llm_client);
        let reply = rt
            .handle_operator_text("Use /propose_draft DIGEST-20990101-000001, 1")
            .await;
        // Digest missing → slash error/path reply, never freeform model text.
        assert!(!reply.contains("FREEFORM_HIT"), "{reply}");
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
    }
}

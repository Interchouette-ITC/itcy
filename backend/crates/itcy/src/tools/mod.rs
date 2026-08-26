// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Runtime tools for freeform / load / draft (`corpus_search`, `draft_status`, `web_search`, `browse_url`).

mod browse;
mod corpus;
mod draft;
mod serp;
mod session;

pub use browse::{resolve_host_browser_cmd, HostBrowser};
pub use corpus::CorpusSearch;
pub use draft::{
    format_stored_draft_status, lookup_draft_status, operator_draft_status_reply,
    parse_draft_id_arg,
};
pub use session::{draft_writer_policy, ResearchSession, ToolPolicy};

use crate::llm::agent::ToolProvider;
use crate::llm::client::{LlmError, LlmToolDef};
use crate::sources::embed::EmbedClient;
use crate::sources::handles::HandlesIndex;
use async_trait::async_trait;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Combined tool provider used by Slack freeform + draft.
pub struct ItcyTools {
    state_db: PathBuf,
    corpus: CorpusSearch,
    handles: Arc<RwLock<HandlesIndex>>,
    host_browser: Mutex<Option<Arc<HostBrowser>>>,
    host_browser_cmd: PathBuf,
    session: Mutex<Option<ResearchSession>>,
    policy: Mutex<ToolPolicy>,
}

impl ItcyTools {
    #[must_use]
    pub fn new(db_path: PathBuf, embed: Arc<dyn EmbedClient>, host_browser_cmd: PathBuf) -> Self {
        let handles = crate::sources::handles::load_handles().unwrap_or_default();
        Self {
            state_db: db_path.clone(),
            corpus: CorpusSearch::new(db_path, embed),
            handles: Arc::new(RwLock::new(handles)),
            host_browser: Mutex::new(None),
            host_browser_cmd,
            session: Mutex::new(None),
            policy: Mutex::new(ToolPolicy::default()),
        }
    }

    /// Snapshot of the in-memory handle registry (loaded at boot; updated by `/handle_add`).
    #[must_use]
    pub fn handles_index(&self) -> HandlesIndex {
        self.handles
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Parse + append `handles.toml` + hot-reload memory (no process restart).
    ///
    /// # Errors
    ///
    /// Returns an operator-facing message on parse / path / IO failure.
    pub fn handle_add(
        &self,
        raw: &str,
    ) -> Result<crate::sources::handles::HandleAddOutcome, String> {
        let path = crate::sources::handles::resolve_handles_path().ok_or_else(|| {
            "handles.toml not found (expected under backend/handles.toml)".to_string()
        })?;
        let mut guard = self
            .handles
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::sources::handles::apply_handle_add(&mut guard, &path, raw)
    }

    /// Start one folder for the whole load→draft research run under `pw/screenshots/<draft_id>/`.
    /// Reuses an existing open session (idempotent) so Slack can open it before LOAD.
    ///
    /// # Errors
    ///
    /// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
    pub async fn begin_research_session(
        &self,
        subject: &str,
        draft_id: &str,
    ) -> Result<PathBuf, LlmError> {
        {
            let guard = self.session.lock().await;
            if let Some(ref s) = *guard {
                return Ok(s.root.clone());
            }
        }
        let root = browse::resolve_pw_screenshots_dir_pub();
        let _ = std::fs::create_dir_all(&root);
        let sess = ResearchSession::start(&root, subject, draft_id)
            .map_err(|e| LlmError::ToolProvider(format!("research session start: {e}")))?;
        let dir = sess.root.clone();
        *self.session.lock().await = Some(sess);
        *self.policy.lock().await = ToolPolicy::default();
        Ok(dir)
    }

    /// Draft ID for the open research session, if any.
    pub async fn session_draft_id(&self) -> Option<String> {
        self.session
            .lock()
            .await
            .as_ref()
            .map(|s| s.draft_id.clone())
    }

    /// Writer turn when the subject already has an https URL: no tools (reuse that URL; stay on subject).
    pub async fn set_subject_https_writer_policy(&self) {
        self.set_subject_https_writer_policy_labeled("TWEET").await;
    }

    /// `LinkedIn` draft writer when the brief already has a cite URL (digest propose, `/draft_about`).
    pub async fn set_draft_subject_https_writer_policy(&self) {
        self.set_subject_https_writer_policy_labeled("DRAFT locked")
            .await;
    }

    async fn set_subject_https_writer_policy_labeled(&self, phase: &str) {
        *self.policy.lock().await = ToolPolicy {
            allow_web_search: false,
            require_browse_before_research: false,
            pack_url_allowlist: Vec::new(),
        };
        if let Some(ref s) = *self.session.lock().await {
            s.clear_extracted_gate();
            s.append_story(&format!(
                "phase: {phase}\nallow_web_search: false\n(subject has https; writer has no tools)\n",
            ));
        }
    }

    /// Draft writer: when LOAD already has publisher URLs, refuse a second `web_search`
    /// (stops homonym topic drift) and allow `browse_url` only on those pack URLs.
    pub async fn set_draft_policy(&self, pack_urls: &[String]) {
        let policy = draft_writer_policy(pack_urls);
        let allow = policy.allow_web_search;
        *self.policy.lock().await = policy;
        if let Some(ref s) = *self.session.lock().await {
            s.clear_extracted_gate();
            s.append_story(&format!(
                "phase: DRAFT\nallow_web_search: {allow}\npack_urls: {}\n",
                pack_urls.len()
            ));
        }
    }

    pub async fn end_research_session(&self, note: &str) {
        let taken = self.session.lock().await.take();
        if let Some(s) = taken {
            s.finish(note);
        }
    }

    pub async fn session_browsed_urls(&self) -> Vec<String> {
        let guard = self.session.lock().await;
        guard
            .as_ref()
            .map(ResearchSession::browsed_urls)
            .unwrap_or_default()
    }

    pub async fn session_extracted_urls(&self) -> Vec<String> {
        let guard = self.session.lock().await;
        guard
            .as_ref()
            .map(ResearchSession::extracted_urls)
            .unwrap_or_default()
    }

    /// Merge URLs into the active research session EXTRACTED list (refill after probe).
    pub async fn session_record_extracted_urls(&self, urls: &[String]) {
        let guard = self.session.lock().await;
        if let Some(s) = guard.as_ref() {
            s.record_extracted_urls(urls);
        }
    }

    pub async fn session_browse_excerpts(&self) -> Vec<(String, String)> {
        let guard = self.session.lock().await;
        guard
            .as_ref()
            .map(ResearchSession::browse_excerpts)
            .unwrap_or_default()
    }

    /// Browse one URL into the active research session (LOAD fallback when the model skipped browse).
    ///
    /// # Errors
    ///
    /// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
    pub async fn research_browse(&self, url: &str) -> Result<String, LlmError> {
        let args = serde_json::json!({ "url": url }).to_string();
        self.browse_url(&args).await
    }

    /// One Brave All+News search into the active research session (short LOAD).
    ///
    /// # Errors
    ///
    /// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
    pub async fn research_web_search(&self, query: &str) -> Result<String, LlmError> {
        let args = serde_json::json!({ "query": query }).to_string();
        self.web_search(&args).await
    }

    async fn ensure_playwright(&self) -> Result<(), LlmError> {
        {
            let guard = self.host_browser.lock().await;
            if guard.is_some() {
                return Ok(());
            }
        }
        let client = Arc::new(HostBrowser::spawn(&self.host_browser_cmd).await?);
        let mut guard = self.host_browser.lock().await;
        if guard.is_none() {
            *guard = Some(client);
        }
        drop(guard);
        Ok(())
    }

    /// Spawn the host browser bridge (if needed) and open Brave/Chromium; keep the child alive.
    ///
    /// # Errors
    ///
    /// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
    pub async fn warmup_browse(&self) -> Result<(), LlmError> {
        self.ensure_playwright().await?;
        let mcp = {
            let guard = self.host_browser.lock().await;
            guard.as_ref().cloned()
        };
        let Some(mcp) = mcp else {
            return Err(LlmError::ToolProvider(
                "host browser failed to start during warmup".into(),
            ));
        };
        mcp.warmup().await
    }

    async fn browse_url(&self, arguments: &str) -> Result<String, LlmError> {
        let url = parse_url_arg(arguments)?;
        {
            let policy = self.policy.lock().await;
            refuse_browse_off_pack(&url, &policy.pack_url_allowlist)?;
        }
        self.ensure_playwright().await?;
        let step_dir = {
            let guard = self.session.lock().await;
            guard.as_ref().map(|s| s.next_step_dir("browse"))
        };
        let mcp = {
            let guard = self.host_browser.lock().await;
            guard.as_ref().cloned()
        };
        let Some(mcp) = mcp else {
            return Err(LlmError::ToolProvider(
                "host browser failed to start".into(),
            ));
        };
        match mcp.browse_url(&url, step_dir.as_deref()).await {
            Ok(out) => {
                if let Some(dir) = step_dir.as_ref() {
                    let _ = std::fs::write(dir.join("tool_result.txt"), &out);
                }
                let final_url = out.lines().find_map(|l| {
                    l.strip_prefix("final_url=")
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                });
                if let Some(ref s) = *self.session.lock().await {
                    s.record_browse(final_url.as_deref());
                    s.record_browse_excerpt(
                        final_url.as_deref().unwrap_or(&url),
                        &truncate_for_story(&out, 2800),
                    );
                    s.append_story(&format!(
                        "step: browse_url\nopened: {url}\nfinal_url: {}\nresult_chars: {}\n--- tool result (truncated) ---\n{}\n---\n",
                        final_url.as_deref().unwrap_or("(none)"),
                        out.len(),
                        truncate_for_story(&out, 2500)
                    ));
                }
                Ok(out)
            }
            Err(e) => {
                if let Some(ref s) = *self.session.lock().await {
                    s.append_story(&format!("step: browse_url\nopened: {url}\nFAILED: {e}\n"));
                }
                Err(e)
            }
        }
    }

    async fn web_search(&self, arguments: &str) -> Result<String, LlmError> {
        let query = parse_query_arg(arguments)?;
        let policy = self.policy.lock().await.clone();
        if !policy.allow_web_search {
            return Err(LlmError::ToolProvider(
                "web_search refused: ResearchPack already has publisher URLs. \
browse_url those (or write). Do not search the same subject again."
                    .into(),
            ));
        }
        {
            let sess = self.session.lock().await;
            if let Some(ref s) = *sess {
                if policy.require_browse_before_research && s.last_search_had_extracted() {
                    return Err(LlmError::ToolProvider(
                        "web_search refused: previous search already returned EXTRACTED links. \
Call browse_url on an on-topic publisher link before searching again."
                            .into(),
                    ));
                }
            }
        }
        self.ensure_playwright().await?;
        let step_dir = {
            let guard = self.session.lock().await;
            guard.as_ref().map(|s| s.next_step_dir("web_search"))
        };
        let mcp = {
            let guard = self.host_browser.lock().await;
            guard.as_ref().cloned()
        };
        let Some(mcp) = mcp else {
            return Err(LlmError::ToolProvider(
                "host browser failed to start".into(),
            ));
        };
        match mcp.web_search(&query, step_dir.as_deref()).await {
            Ok(out) => {
                let had = out.contains("url=https://")
                    && (out.contains("[other-publisher]")
                        || out.contains("[news-publisher]")
                        || out.contains("[web-publisher]")
                        || out.contains("MERGED ranked"));
                let extracted = extract_publisher_urls_from_tool_result(&out);
                if let Some(ref s) = *self.session.lock().await {
                    s.record_web_search(had);
                    s.record_extracted_urls(&extracted);
                    s.append_story(&format!(
                        "step: web_search\nquery: {query}\nhad_extracted: {had}\nextracted: {}\nresult_chars: {}\n--- tool result (truncated) ---\n{}\n---\n",
                        extracted.len(),
                        out.len(),
                        truncate_for_story(&out, 3500)
                    ));
                }
                info!(query = %query, had_extracted = had, extracted = extracted.len(), "tools: web_search recorded");
                Ok(out)
            }
            Err(e) => {
                if let Some(ref s) = *self.session.lock().await {
                    s.record_web_search(false);
                    s.append_story(&format!(
                        "step: web_search\nquery: {query}\nhad_extracted: false\nFAILED: {e}\n"
                    ));
                }
                Err(e)
            }
        }
    }

    /// Look up `LinkedIn` and X handles for an entity name from the in-memory registry.
    fn lookup_handles(&self, arguments: &str) -> String {
        let Some(name) = parse_name_arg(arguments) else {
            return "lookup_handles requires {\"name\": \"...\"}".to_string();
        };
        let index = self.handles_index();
        let matches = index.search(&name);
        if matches.is_empty() {
            return format!("No handles found for \"{name}\" in registry.");
        }
        let mut out = format!("lookup_handles result for \"{name}\":\n");
        for entry in matches {
            let best_for_linkedin = if !entry.linkedin.is_empty() {
                entry.linkedin.as_str()
            } else if !entry.x.is_empty() {
                entry.x.as_str()
            } else {
                ""
            };
            let best_for_x = if !entry.x.is_empty() {
                entry.x.as_str()
            } else if !entry.linkedin.is_empty() {
                entry.linkedin.as_str()
            } else {
                ""
            };
            let _ = writeln!(out, "- name: {}", entry.name);
            if !entry.linkedin.is_empty() {
                let _ = writeln!(out, "  linkedin: {}", entry.linkedin);
            }
            if !entry.x.is_empty() {
                let _ = writeln!(out, "  x: {}", entry.x);
            }
            if !entry.linkedin_url.is_empty() {
                let _ = writeln!(out, "  linkedin_url: {}", entry.linkedin_url);
            }
            if !entry.x_url.is_empty() {
                let _ = writeln!(out, "  x_url: {}", entry.x_url);
            }
            if !best_for_linkedin.is_empty() {
                let _ = writeln!(out, "  best_for_linkedin: {best_for_linkedin}");
            }
            if !best_for_x.is_empty() {
                let _ = writeln!(out, "  best_for_x: {best_for_x}");
            }
        }
        out
    }
}

fn truncate_for_story(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let cut: String = t.chars().take(max).collect();
    format!("{cut}\n…(truncated)")
}

fn extract_publisher_urls_from_tool_result(out: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for line in out.lines() {
        if let Some(rest) = line.trim().strip_prefix("url=") {
            let u = rest.trim();
            if (u.starts_with("https://") || u.starts_with("http://"))
                && !urls.iter().any(|x| x == u)
            {
                urls.push(u.to_string());
            }
        } else if let Some(idx) = line.find("url=") {
            let u = line[idx + 4..].trim();
            let u = u
                .split_whitespace()
                .next()
                .unwrap_or(u)
                .trim_end_matches(['|', ',', ';']);
            if (u.starts_with("https://") || u.starts_with("http://"))
                && !urls.iter().any(|x| x == u)
            {
                urls.push(u.to_string());
            }
        }
    }
    urls
}

fn refuse_browse_off_pack(url: &str, allow: &[String]) -> Result<(), LlmError> {
    if allow.is_empty() || crate::sources::url_hygiene::url_in_allowlist(url, allow) {
        return Ok(());
    }
    Err(LlmError::ToolProvider(format!(
        "browse_url refused: `{url}` is not in the ResearchPack. \
Browse a pack URL or write the post. Do not open a different story."
    )))
}

fn parse_name_arg(arguments: &str) -> Option<String> {
    let v: serde_json::Value =
        serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!({}));
    v.get("name")
        .and_then(|n| n.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn parse_url_arg(arguments: &str) -> Result<String, LlmError> {
    let v: serde_json::Value =
        serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!({}));
    let url = v
        .get("url")
        .and_then(|u| u.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| LlmError::ToolProvider("browse_url requires {\"url\": \"...\"}".into()))?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(LlmError::ToolProvider(
            "browse_url: only http(s) public URLs are allowed".into(),
        ));
    }
    let lower = url.to_ascii_lowercase();
    if crate::sources::url_hygiene::is_linkedin_host(&lower)
        || lower.contains("instagram.com")
        || lower.contains("facebook.com")
        || lower.contains("tiktok.com")
    {
        return Err(LlmError::ToolProvider(format!(
            "browse_url: social URL forbidden for research (`{url}`). \
Use a non-LinkedIn / non-Instagram publisher https URL from web_search EXTRACTED."
        )));
    }
    if crate::sources::url_hygiene::is_junk_or_search_url(url) {
        return Err(LlmError::ToolProvider(format!(
            "browse_url: refused non-publisher / placeholder URL `{url}` - use a real article https link from web_search EXTRACTED"
        )));
    }
    Ok(url.to_string())
}

fn parse_query_arg(arguments: &str) -> Result<String, LlmError> {
    let v: serde_json::Value =
        serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!({}));
    v.get("query")
        .and_then(|q| q.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| LlmError::ToolProvider("web_search requires {\"query\": \"...\"}".into()))
}

fn tool_defs() -> Vec<LlmToolDef> {
    vec![
        LlmToolDef {
            name: "corpus_search".into(),
            description: "Search ITCy LinkedIn / sources corpus (SQLite). Voice and history only \
(already ingested / enriched - do NOT browse_url anything from this tool). \
Return ON-TOPIC hits for tone. Corpus alone is NOT the news article - \
use web_search + browse_url on EXTRACTED publisher URLs for external articles. \
LinkedIn URLs in hits are redacted."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Subject or keywords to search"
                    }
                },
                "required": ["query"]
            }),
        },
        LlmToolDef {
            name: "web_search".into(),
            description: "Search via host browser (Brave preferred). ONE call scrapes All (web, source=web&summary=1) \
then News (source=web). Returns AI_OVERVIEW per scope + EXTRACTED from News + All + MERGED ranked list \
(news/articles preferred over directories). LinkedIn never returned. Call ONCE then browse_url 1-2 MERGED URLs \
(prefer analysis articles). Do not search again until browsed (tool will refuse). Never invent URLs."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query, e.g. company + topic + news"
                    }
                },
                "required": ["query"]
            }),
        },
        LlmToolDef {
            name: "browse_url".into(),
            description: "Open a public http(s) publisher URL in the host browser; return page text + final_url. \
Use after web_search on EXTRACTED links. Forbidden: LinkedIn (any), YouTube, Reddit, SERP URLs."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Public https URL to open"
                    }
                },
                "required": ["url"]
            }),
        },
        LlmToolDef {
            name: "draft_status".into(),
            description: "Authoritative Draft lifecycle from runtime.db. REQUIRED before saying \
anything about a DRAFT-… id (pending/open/accepted/published). Never invent status."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "draft_id": {
                        "type": "string",
                        "description": "Draft id DRAFT-YYYYMMDD-NNNNNN"
                    }
                },
                "required": ["draft_id"]
            }),
        },
        LlmToolDef {
            name: "lookup_handles".into(),
            description: "Look up the LinkedIn and X/Twitter handles for a named entity \
(company, person, project) from a curated registry. \
Call this when you are about to mention a known entity by name and want its @handle \
for a LinkedIn post or X tweet. Returns up to 5 matches; empty when not found. \
Never invent a handle not returned by this tool."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Entity name to look up, e.g. \"Anthropic\" or \"Rust Foundation\""
                    }
                },
                "required": ["name"]
            }),
        },
    ]
}

#[async_trait]
impl ToolProvider for ItcyTools {
    async fn list_tools(&self) -> Result<Vec<LlmToolDef>, LlmError> {
        Ok(tool_defs())
    }

    async fn call_tool(&self, name: &str, arguments: &str) -> Result<String, LlmError> {
        match name {
            "corpus_search" => self.corpus.search(arguments).await,
            "draft_status" => {
                let id = parse_draft_id_arg(arguments)?;
                lookup_draft_status(&self.state_db, &id)
            }
            "web_search" => match self.web_search(arguments).await {
                Ok(s) => Ok(s),
                Err(e) => {
                    warn!(error = %e, "tools: web_search failed");
                    Err(e)
                }
            },
            "browse_url" => match self.browse_url(arguments).await {
                Ok(s) => Ok(s),
                Err(e) => {
                    warn!(error = %e, "tools: browse_url failed");
                    Err(e)
                }
            },
            "lookup_handles" => Ok(self.lookup_handles(arguments)),
            other => Err(LlmError::ToolProvider(format!("unknown tool: {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_ok() {
        assert_eq!(
            parse_url_arg(r#"{"url":"https://news.publisher.test/a"}"#).unwrap(),
            "https://news.publisher.test/a"
        );
    }

    #[test]
    fn parse_url_rejects_non_http() {
        assert!(parse_url_arg(r#"{"url":"file:///etc/passwd"}"#).is_err());
    }

    #[test]
    fn parse_url_rejects_all_linkedin() {
        for url in [
            "https://www.linkedin.com/in/someone/",
            "https://www.linkedin.com/uas/login",
            "https://www.linkedin.com/search/results/companies?keywords=x",
            "https://www.linkedin.com/company/foo/",
            "https://lnkd.in/eBxvGMdh",
        ] {
            let arg = format!(r#"{{"url":"{url}"}}"#);
            assert!(
                parse_url_arg(&arg).is_err(),
                "must refuse LinkedIn browse: {url}"
            );
        }
    }

    #[test]
    fn parse_url_rejects_brave_serp() {
        assert!(parse_url_arg(r#"{"url":"https://search.brave.com/search?q=x"}"#).is_err());
    }

    #[test]
    fn parse_query_ok() {
        assert_eq!(
            parse_query_arg(r#"{"query":"company CEO"}"#).unwrap(),
            "company CEO"
        );
    }

    #[test]
    fn tool_defs_include_web_search() {
        let defs = tool_defs();
        let names: Vec<_> = defs.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"browse_url"));
        assert!(names.contains(&"corpus_search"));
    }

    #[test]
    fn draft_writer_policy_locks_search_when_pack_has_urls() {
        let pack = vec!["https://epage.github.io/blog/2026/08/cargo-vision".to_string()];
        let p = draft_writer_policy(&pack);
        assert!(!p.allow_web_search);
        assert_eq!(p.pack_url_allowlist, pack);
        let empty = draft_writer_policy(&[]);
        assert!(empty.allow_web_search);
        assert!(empty.pack_url_allowlist.is_empty());
    }

    #[test]
    fn browse_off_pack_refused_when_allowlist_set() {
        let pack = vec!["https://epage.github.io/blog/2026/08/cargo-vision".to_string()];
        assert!(refuse_browse_off_pack(pack[0].as_str(), &pack).is_ok());
        assert!(refuse_browse_off_pack(
            "https://www.knapp.com/en/insights/blog/ai-trends-logistics-2026/",
            &pack
        )
        .is_err());
        assert!(refuse_browse_off_pack(
            "https://www.knapp.com/en/insights/blog/ai-trends-logistics-2026/",
            &[]
        )
        .is_ok());
    }
}

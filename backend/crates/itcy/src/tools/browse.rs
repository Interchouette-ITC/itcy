// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Host browser bridge for draft research (Brave/Chromium).
//!
//! Spawns a product stdio child (`scripts/playwright-mcp.sh` → `@playwright/mcp`).

use crate::llm::client::LlmError;
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{serve_client, RoleClient};
use serde_json::{Map, Value as JsonValue};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Connected host browser (Brave or Chromium) via the product browse launcher.
pub struct HostBrowser {
    service: Mutex<RunningService<RoleClient, ()>>,
    /// Root for screenshots when no research session step dir is passed.
    screenshots_dir: PathBuf,
}

impl HostBrowser {
    /// Spawns `bash <script>` as the product host-browser stdio bridge.
    ///
    /// # Errors
    ///
    /// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
    pub async fn spawn(script: &Path) -> Result<Self, LlmError> {
        if !script.is_file() {
            return Err(LlmError::ToolProvider(format!(
                "host browser script missing: {}",
                script.display()
            )));
        }
        let screenshots_dir = resolve_pw_screenshots_dir();
        let _ = std::fs::create_dir_all(&screenshots_dir);
        let mut cmd = Command::new("bash");
        cmd.arg(script);
        // Prefer ITCY_PLAYWRIGHT_BROWSERS_PATH or Playwright defaults, not ambient env.
        cmd.env_remove("PLAYWRIGHT_BROWSERS_PATH");
        if let Ok(p) = std::env::var("ITCY_PLAYWRIGHT_BROWSERS_PATH") {
            if !p.trim().is_empty() {
                cmd.env("PLAYWRIGHT_BROWSERS_PATH", p);
            }
        }
        cmd.env("ITCY_PW_SCREENSHOTS_DIR", &screenshots_dir);
        let browser = pw_browser_name();
        cmd.env("ITCY_PW_BROWSER", &browser);
        let profile = resolve_pw_profile_dir();
        let _ = std::fs::create_dir_all(&profile);
        cmd.env("ITCY_PW_USER_DATA_DIR", &profile);
        info!(browser = %browser, profile = %profile.display(), "tools: spawning host browser");
        // Script path must be absolute: we must not chdir before bash finds it.
        let transport = TokioChildProcess::new(cmd).map_err(|e| {
            error!(error = %e, "tools: failed to spawn host browser");
            LlmError::ToolProvider(format!("spawn host browser: {e}"))
        })?;
        let service = serve_client((), transport).await.map_err(|e| {
            error!(error = %e, "tools: host browser initialize failed");
            LlmError::ToolProvider(format!("host browser initialize: {e}"))
        })?;
        info!("tools: host browser ready");
        Ok(Self {
            service: Mutex::new(service),
            screenshots_dir,
        })
    }

    /// Opens the configured browser once (navigate `about:blank` + snapshot) so the first real browse is warmer.
    ///
    /// # Errors
    ///
    /// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
    pub async fn warmup(&self) -> Result<(), LlmError> {
        self.call_mcp(
            "browser_navigate",
            object_args(&[("url", JsonValue::String("about:blank".into()))]),
        )
        .await?;
        let _ = self.call_mcp("browser_snapshot", object_args(&[])).await?;
        Ok(())
    }

    /// Navigate then snapshot; return text for the model (includes final URL after redirects).
    /// `step_dir`: research-session step folder; else a loose timestamp dir under screenshots root.
    ///
    /// # Errors
    ///
    /// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
    pub async fn browse_url(&self, url: &str, step_dir: Option<&Path>) -> Result<String, LlmError> {
        let round = self.begin_round("browse", step_dir);
        info!(opened = %url, "tools: browse_url");
        self.call_mcp(
            "browser_navigate",
            object_args(&[("url", JsonValue::String(url.to_string()))]),
        )
        .await?;
        // Shorteners / LinkedIn redirects often need a beat before location.href settles.
        let _ = self
            .call_mcp(
                "browser_wait_for",
                object_args(&[("time", JsonValue::from(2))]),
            )
            .await;
        let eval_raw = self
            .call_mcp(
                "browser_evaluate",
                object_args(&[("function", JsonValue::String("() => location.href".into()))]),
            )
            .await
            .unwrap_or_default();
        let final_url = normalize_evaluated_url(&eval_raw).unwrap_or_else(|| url.to_string());
        let shot = self
            .capture_round_screenshot(&round, "01-page")
            .await
            .unwrap_or_default();
        self.close_other_browser_tabs().await;
        let snap = self.call_mcp("browser_snapshot", object_args(&[])).await?;
        // Small models drown in huge a11y trees and rewrite the post as a page summary.
        let trimmed: String = snap.chars().take(3_500).collect();
        write_round_meta(
            &round,
            &[
                ("kind", "browse_url"),
                ("opened", url),
                ("final_url", &final_url),
                ("browser", &pw_browser_name()),
                ("screenshot", &shot),
                ("snapshot_chars", &trimmed.len().to_string()),
            ],
        );
        if trimmed.trim().is_empty() {
            return Err(LlmError::ToolProvider(
                "browse_url: empty snapshot after navigate".into(),
            ));
        }
        if snapshot_looks_like_not_found(&trimmed) {
            warn!(
                opened = %url,
                final_url = %final_url,
                "tools: browse_url page not found"
            );
            return Err(LlmError::ToolProvider(format!(
                "browse_url: page not found (404) for opened={url} final_url={final_url}"
            )));
        }
        if is_short_link_url(&final_url) {
            info!(
                opened = %url,
                final_url = %final_url,
                "tools: browse_url short-link unresolved"
            );
            return Ok(format!(
                "browse_url opened={url}\nfinal_url={final_url}\n\
SHORT_LINK_UNRESOLVED: this is still a shortener (lnkd.in / bit.ly / …). \
Do NOT put it in the ResearchPack or LinkedIn post. \
Find the canonical publisher https URL via web_search (title/company + topic), then browse_url that URL.\n\n\
Page text / snapshot (may be a login wall):\n{trimmed}"
            ));
        }
        if is_linkedin_login_wall(&final_url)
            || crate::sources::url_hygiene::is_linkedin_host(&final_url)
            || final_url.to_ascii_lowercase().contains("instagram.com")
        {
            warn!(opened = %url, final_url = %final_url, "tools: browse_url social forbidden");
            return Ok(format!(
                "browse_url opened={url}\nfinal_url={final_url}\n\
SOCIAL_FORBIDDEN: LinkedIn/Instagram are not research sources. Do NOT cite this URL.\n\n\
Page text / snapshot:\n{trimmed}"
            ));
        }
        info!(opened = %url, final_url = %final_url, "tools: browse_url ok");
        Ok(format!(
            "browse_url opened={url}\nfinal_url={final_url}\n\
SUPPORT ONLY: use facts from this page to support the OPERATOR SUBJECT. \
Do NOT rewrite the post as a summary of this page. Do NOT change the subject to match this page. \
Cite final_url only if it is ON-TOPIC for the operator request.\n\n\
Page text / snapshot (truncated):\n{trimmed}"
        ))
    }

    /// Public web search: **`DuckDuckGo`** organic links + **Brave** AI overview (no Brave News).
    ///
    /// # Errors
    ///
    /// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
    pub async fn web_search(
        &self,
        query: &str,
        step_dir: Option<&Path>,
    ) -> Result<String, LlmError> {
        let q = query.trim();
        if q.is_empty() {
            return Err(LlmError::ToolProvider(
                "web_search requires a non-empty query".into(),
            ));
        }
        let round = self.begin_round("web_search", step_dir);

        let ddg_attempt = self
            .dom_serp_once(q, "duckduckgo", SerpScope::All, &round, "01-ddg")
            .await?;
        let brave_engine = serp_engine_name();
        let brave_attempt = self
            .dom_serp_once(q, &brave_engine, SerpScope::All, &round, "02-brave-ai")
            .await?;

        let merged =
            crate::tools::serp::merge_ddg_and_brave_links(&ddg_attempt.links, &brave_attempt.links);
        let merged_show: Vec<_> = merged.into_iter().take(10).collect();

        write_web_search_artifacts(&round, q, &ddg_attempt, &brave_attempt, &merged_show);

        let browser = pw_browser_name();
        if let Some(err) = web_search_empty_error(
            q,
            &browser,
            &round,
            &ddg_attempt,
            &brave_attempt,
            merged_show.is_empty(),
        ) {
            return Err(err);
        }

        info!(
            query = %q,
            browser = %browser,
            ddg_links = ddg_attempt.links.len(),
            brave_links = brave_attempt.links.len(),
            merged = merged_show.len(),
            ai_chars = brave_attempt.ai_overview.len(),
            "tools: web_search ok (DDG links + Brave AI overview)"
        );

        let tool_out = format_web_search_tool_out(
            q,
            &browser,
            &round,
            &ddg_attempt,
            &brave_attempt,
            &merged_show,
        );
        write_step_text(&round, "tool_result.txt", &tool_out);
        Ok(tool_out)
    }

    /// One SERP navigate+extract+screenshot. `label` prefixes artifacts (`01-web`, `02-news`, …).
    async fn dom_serp_once(
        &self,
        q: &str,
        serp_engine: &str,
        scope: SerpScope,
        round: &Path,
        label: &str,
    ) -> Result<DomSerpAttempt, LlmError> {
        let search_url = serp_search_url(serp_engine, scope, q);
        info!(
            query = %q,
            serp_engine = %serp_engine,
            scope = "web",
            label = %label,
            url = %search_url,
            "tools: web_search DOM"
        );
        if let Err(e) = self
            .call_mcp(
                "browser_navigate",
                object_args(&[("url", JsonValue::String(search_url.clone()))]),
            )
            .await
        {
            warn!(error = %e, scope = "web", "tools: web_search DOM navigate failed");
            return Ok(DomSerpAttempt::blocked_empty(search_url));
        }
        // All+summary cards often hydrate after first paint.
        let wait_secs = 4;
        let _ = self
            .call_mcp(
                "browser_wait_for",
                object_args(&[("time", JsonValue::from(wait_secs))]),
            )
            .await;

        self.close_other_browser_tabs().await;
        let snap = self
            .call_mcp("browser_snapshot", object_args(&[]))
            .await
            .unwrap_or_default();
        write_step_text(round, &format!("{label}-snapshot.txt"), &snap);

        let extract_js = serp_extract_js(serp_engine);
        let eval_raw = self
            .call_mcp(
                "browser_evaluate",
                object_args(&[("function", JsonValue::String(extract_js))]),
            )
            .await
            .unwrap_or_else(|_| {
                r#"{"blocked":false,"links":[],"href":"","ai_overview":""}"#.into()
            });
        write_step_text(round, &format!("{label}-evaluate.txt"), &eval_raw);

        let shot = self
            .capture_round_screenshot(round, &format!("{label}-serp"))
            .await
            .unwrap_or_default();

        let ev = crate::tools::serp::split_serp_evaluate(&eval_raw);
        // NEVER run looks_like_*_block on raw eval_raw: the browse helper appends the JS we sent,
        // and that JS contains captcha detector strings → false "SERP blocked" with real links.
        let prelim_links = crate::tools::serp::extract_serp_links(&ev.links_json, "");
        let mut blocked = serp_blocked_from_signals(serp_engine, ev.blocked, &snap, &ev.href);

        let page_html = self
            .fetch_serp_page_html(blocked || prelim_links.is_empty())
            .await;
        if !page_html.is_empty() {
            write_step_text(
                round,
                &format!("{label}-html.txt"),
                &page_html.chars().take(50_000).collect::<String>(),
            );
        }
        blocked = apply_html_block_signals(serp_engine, blocked, &page_html);

        let (links, blocked) = resolve_serp_links(
            prelim_links,
            ev.blocked,
            blocked,
            &ev.links_json,
            &page_html,
        );
        let ai_overview = if blocked && links.is_empty() {
            String::new()
        } else {
            ev.ai_overview
        };
        let page_href = if ev.href.is_empty() {
            search_url.clone()
        } else {
            ev.href
        };

        if blocked {
            warn!(
                serp_engine = %serp_engine,
                scope = "web",
                page_href = %page_href,
                "tools: SERP interstitial/block detected"
            );
        }

        Ok(DomSerpAttempt {
            blocked,
            search_url,
            page_href,
            ai_overview,
            links,
            shot,
        })
    }

    async fn fetch_serp_page_html(&self, need_html: bool) -> String {
        if !need_html {
            return String::new();
        }
        self.call_mcp(
            "browser_evaluate",
            object_args(&[(
                "function",
                JsonValue::String(
                    "() => document.documentElement.outerHTML.slice(0, 200000)".into(),
                ),
            )]),
        )
        .await
        .unwrap_or_default()
    }

    fn begin_round(&self, kind: &str, step_dir: Option<&Path>) -> PathBuf {
        if let Some(dir) = step_dir {
            let _ = std::fs::create_dir_all(dir);
            return dir.to_path_buf();
        }
        let stamp = round_stamp();
        let dir = self.screenshots_dir.join(format!("{stamp}-{kind}"));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(error = %e, dir = %dir.display(), "tools: could not create screenshot round dir");
        }
        dir
    }

    /// MCP may write under `pw/mcp` or the product cwd (`backend/`). Find staging, copy into session step.
    async fn capture_round_screenshot(
        &self,
        round: &Path,
        label: &str,
    ) -> Result<String, LlmError> {
        let _ = std::fs::create_dir_all(round);
        let dest = round.join(format!("{label}.png"));
        let mcp_dir = resolve_pw_mcp_dir();
        let _ = std::fs::create_dir_all(&mcp_dir);
        // Absolute path under pw/mcp only. Relative names land in the browse-helper cwd
        // (product root) and litter itcy-*.png at checkout root.
        let staging_path = mcp_dir.join(format!(
            "itcy-{}-{}.png",
            round_stamp(),
            label.replace('/', "-")
        ));
        let staging_name = staging_path.display().to_string();
        match self
            .call_mcp(
                "browser_take_screenshot",
                object_args(&[
                    ("type", JsonValue::String("png".into())),
                    ("scale", JsonValue::String("css".into())),
                    ("fullPage", JsonValue::Bool(true)),
                    ("filename", JsonValue::String(staging_name.clone())),
                ]),
            )
            .await
        {
            Ok(msg) => {
                let basename = staging_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("screenshot.png");
                let src = find_screenshot_staging(&staging_name, &mcp_dir, &msg)
                    .or_else(|| find_screenshot_staging(basename, &mcp_dir, &msg));
                if let Some(src) = src {
                    if let Err(e) = std::fs::copy(&src, &dest) {
                        warn!(error = %e, "tools: screenshot copy into session failed");
                        return Err(LlmError::ToolProvider(format!(
                            "screenshot copy failed: {e}"
                        )));
                    }
                    let _ = std::fs::remove_file(&src);
                    // Sweep accidental relative dumps at product / backend cwd.
                    sweep_orphan_screenshot_names(Some(basename));
                    info!(screenshot = %dest.display(), "tools: screenshot saved");
                    return Ok(dest.display().to_string());
                }
                warn!(
                    staging = %staging_name,
                    detail = %msg.chars().take(120).collect::<String>(),
                    "tools: screenshot staging file missing"
                );
                Err(LlmError::ToolProvider(
                    "screenshot staging file missing".into(),
                ))
            }
            Err(e) => {
                warn!(error = %e, "tools: screenshot failed");
                Err(e)
            }
        }
    }

    /// Close every Brave/Playwright tab except the current one so snapshots do not list leftovers.
    async fn close_other_browser_tabs(&self) {
        let list = match self
            .call_mcp(
                "browser_tabs",
                object_args(&[("action", JsonValue::String("list".into()))]),
            )
            .await
        {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "tools: browser_tabs list failed; leaving tabs as-is");
                return;
            }
        };
        let to_close = tab_indices_to_close(&list);
        if to_close.is_empty() {
            return;
        }
        info!(
            closing = to_close.len(),
            "tools: closing leftover browser tabs"
        );
        for index in to_close {
            if let Err(e) = self
                .call_mcp(
                    "browser_tabs",
                    object_args(&[
                        ("action", JsonValue::String("close".into())),
                        ("index", JsonValue::from(index)),
                    ]),
                )
                .await
            {
                warn!(index, error = %e, "tools: browser_tabs close failed");
            }
        }
    }

    async fn call_mcp(
        &self,
        name: &str,
        arguments: Option<Map<String, JsonValue>>,
    ) -> Result<String, LlmError> {
        let guard = self.service.lock().await;
        let mut params = CallToolRequestParams::new(name.to_string());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        let result = guard.peer().call_tool(params).await.map_err(|e| {
            error!(tool = %name, error = %e, "tools: host browser tools/call failed");
            LlmError::ToolProvider(format!("host browser {name}: {e}"))
        })?;
        drop(guard);
        let text = call_tool_result_to_string(&result);
        if result.is_error.unwrap_or(false) || looks_like_mcp_browser_error(&text) {
            error!(tool = %name, detail = %text.chars().take(200).collect::<String>(), "tools: host browser tool returned error");
            return Err(LlmError::ToolProvider(format!(
                "host browser {name}: {text}"
            )));
        }
        Ok(text)
    }
}

struct DomSerpAttempt {
    blocked: bool,
    search_url: String,
    page_href: String,
    ai_overview: String,
    links: Vec<crate::tools::serp::SerpLink>,
    shot: String,
}

impl DomSerpAttempt {
    const fn blocked_empty(search_url: String) -> Self {
        Self {
            blocked: true,
            search_url,
            page_href: String::new(),
            ai_overview: String::new(),
            links: Vec::new(),
            shot: String::new(),
        }
    }
}

fn serp_blocked_from_signals(
    serp_engine: &str,
    eval_blocked: bool,
    snap: &str,
    href: &str,
) -> bool {
    let mut blocked = eval_blocked;
    if serp_engine == "brave" {
        blocked = blocked
            || crate::tools::serp::looks_like_brave_block(snap)
            || crate::tools::serp::looks_like_brave_block(href);
    }
    if serp_engine == "google" {
        blocked = blocked
            || crate::tools::serp::looks_like_google_block(href)
            || crate::tools::serp::looks_like_google_block(snap);
    }
    if serp_engine == "duckduckgo" {
        blocked = blocked
            || crate::tools::serp::looks_like_ddg_block(href)
            || crate::tools::serp::looks_like_ddg_block(snap);
    }
    blocked
}

fn apply_html_block_signals(serp_engine: &str, blocked: bool, page_html: &str) -> bool {
    blocked
        || (serp_engine == "brave" && crate::tools::serp::looks_like_brave_block(page_html))
        || (serp_engine == "google" && crate::tools::serp::looks_like_google_block(page_html))
        || (serp_engine == "duckduckgo" && crate::tools::serp::looks_like_ddg_block(page_html))
}

fn resolve_serp_links(
    prelim_links: Vec<crate::tools::serp::SerpLink>,
    eval_blocked: bool,
    blocked: bool,
    links_json: &str,
    page_html: &str,
) -> (Vec<crate::tools::serp::SerpLink>, bool) {
    if !prelim_links.is_empty() && !eval_blocked {
        return (prelim_links, false);
    }
    if blocked {
        return (Vec::new(), blocked);
    }
    (
        crate::tools::serp::extract_serp_links(links_json, page_html),
        blocked,
    )
}

fn ai_overview_or_none(raw: &str) -> String {
    if raw.trim().is_empty() {
        "(none)".to_string()
    } else {
        raw.to_string()
    }
}

fn write_web_search_artifacts(
    round: &Path,
    q: &str,
    ddg_attempt: &DomSerpAttempt,
    brave_attempt: &DomSerpAttempt,
    merged_show: &[crate::tools::serp::SerpLink],
) {
    let formatted_ddg = crate::tools::serp::format_serp_links_labeled(&ddg_attempt.links, "ddg");
    let formatted_brave =
        crate::tools::serp::format_serp_links_labeled(&brave_attempt.links, "brave-fallback");
    let formatted_merged = crate::tools::serp::format_serp_links(merged_show);
    let ai_brave = ai_overview_or_none(&brave_attempt.ai_overview);

    write_round_meta(
        round,
        &[
            ("kind", "web_search"),
            ("query", q),
            ("browser", &pw_browser_name()),
            ("search_url_ddg", &ddg_attempt.search_url),
            ("search_url_brave_ai", &brave_attempt.search_url),
            ("page_href_ddg", &ddg_attempt.page_href),
            ("page_href_brave_ai", &brave_attempt.page_href),
            (
                "dom_blocked_ddg",
                if ddg_attempt.blocked { "true" } else { "false" },
            ),
            (
                "dom_blocked_brave_ai",
                if brave_attempt.blocked {
                    "true"
                } else {
                    "false"
                },
            ),
            (
                "ai_overview_chars",
                &brave_attempt.ai_overview.len().to_string(),
            ),
            ("screenshot_ddg", &ddg_attempt.shot),
            ("screenshot_brave_ai", &brave_attempt.shot),
            ("extracted_merged", &formatted_merged),
        ],
    );
    write_step_text(
        round,
        "search_urls.txt",
        &format!(
            "ddg (organic links):\n{}\n\nbrave (AI overview, summary=1):\n{}\n",
            ddg_attempt.search_url, brave_attempt.search_url
        ),
    );
    write_step_text(round, "ai_overview_brave.txt", &ai_brave);
    write_step_text(round, "extracted_ddg.txt", &formatted_ddg);
    write_step_text(round, "extracted_brave_fallback.txt", &formatted_brave);
    write_step_text(round, "extracted_merged.txt", &formatted_merged);
}

fn web_search_empty_error(
    q: &str,
    browser: &str,
    round: &Path,
    ddg_attempt: &DomSerpAttempt,
    brave_attempt: &DomSerpAttempt,
    merged_empty: bool,
) -> Option<LlmError> {
    if !merged_empty {
        return None;
    }
    let ddg_dead = ddg_attempt.blocked && ddg_attempt.links.is_empty();
    let brave_dead = brave_attempt.blocked && brave_attempt.links.is_empty();
    if ddg_dead && brave_dead {
        error!(
            query = %q,
            browser = %browser,
            "tools: web_search blocked (DDG + Brave empty)"
        );
        let detail = format!(
            "web_search: SERP blocked (duckduckgo+brave in {browser}). \
ddg_href={} brave_href={} round={}. \
Do NOT invent URLs. Leave ResearchPack candidates empty.",
            ddg_attempt.page_href,
            brave_attempt.page_href,
            round.display()
        );
        write_step_text(round, "tool_result.txt", &detail);
        return Some(LlmError::ToolProvider(format!(
            "web_search: SERP blocked (duckduckgo+brave/{browser}). No publisher links."
        )));
    }
    error!(query = %q, "tools: web_search found no links (DDG+Brave)");
    let detail = format!(
        "web_search: no publisher links from DOM (DDG + Brave). \
ddg_href={} brave_href={} round={}. Do NOT invent URLs.",
        ddg_attempt.page_href,
        brave_attempt.page_href,
        round.display()
    );
    write_step_text(round, "tool_result.txt", &detail);
    Some(LlmError::ToolProvider(
        "web_search: no publisher links from DOM.".into(),
    ))
}

fn format_web_search_tool_out(
    q: &str,
    browser: &str,
    round: &Path,
    ddg_attempt: &DomSerpAttempt,
    brave_attempt: &DomSerpAttempt,
    merged_show: &[crate::tools::serp::SerpLink],
) -> String {
    let formatted_ddg = crate::tools::serp::format_serp_links_labeled(&ddg_attempt.links, "ddg");
    let formatted_brave =
        crate::tools::serp::format_serp_links_labeled(&brave_attempt.links, "brave-fallback");
    let formatted_merged = crate::tools::serp::format_serp_links(merged_show);
    let ai_brave = ai_overview_or_none(&brave_attempt.ai_overview);
    format!(
        "web_search query={q}\n\
browser={browser}\n\
Purpose: pick 3-4 ON-TOPIC candidates from MERGED EXTRACTED (DuckDuckGo organic), then browse_url 1-2.\n\
AI_OVERVIEW from Brave (summary=1) is helper context only - still require ON-TOPIC final_url from EXTRACTED + browse_url.\n\
Social URLs stripped from EXTRACTED: LinkedIn / Instagram / Facebook / TikTok.\n\
Never invent URLs. Never cite duckduckgo.com, google.com/search, or search.brave.com.\n\
search_page_ddg={}\npage_href_ddg={}\n\
search_page_brave_ai={}\npage_href_brave_ai={}\n\
dom_blocked_ddg={} dom_blocked_brave_ai={}\n\
round={}\nscreenshot_ddg={}\nscreenshot_brave_ai={}\n\n\
AI_OVERVIEW (Brave web):\n{ai_brave}\n\n\
EXTRACTED links from DuckDuckGo ({n_ddg}):\n{formatted_ddg}\n\
EXTRACTED links from Brave fallback ({n_brave}):\n{formatted_brave}\n\
MERGED ranked candidates ({n_merged}) - prefer these for ResearchPack / browse:\n{formatted_merged}",
        ddg_attempt.search_url,
        ddg_attempt.page_href,
        brave_attempt.search_url,
        brave_attempt.page_href,
        ddg_attempt.blocked,
        brave_attempt.blocked,
        round.display(),
        ddg_attempt.shot,
        brave_attempt.shot,
        n_ddg = ddg_attempt.links.len(),
        n_brave = brave_attempt.links.len(),
        n_merged = merged_show.len(),
    )
}

fn resolve_pw_screenshots_dir() -> PathBuf {
    resolve_pw_screenshots_dir_pub()
}

/// Resolve a subdirectory under repo `pw/` (always the product tree, never cwd `../pw`).
#[must_use]
pub fn resolve_pw_subdir(name: &str) -> PathBuf {
    crate::paths::product_join("pw").join(name)
}

/// Public for research-session folder creation under the same root.
#[must_use]
pub fn resolve_pw_screenshots_dir_pub() -> PathBuf {
    for key in ["ITCY_PW_SCREENSHOTS_DIR", "ITCY_PW_DEBUG_DIR"] {
        if let Ok(p) = std::env::var(key) {
            let t = p.trim();
            if !t.is_empty() {
                return PathBuf::from(t);
            }
        }
    }
    resolve_pw_subdir("screenshots")
}

/// Host-browser `--output-dir` (only path the browse helper may write screenshots into).
fn resolve_pw_mcp_dir() -> PathBuf {
    let dir = std::env::var("ITCY_PW_MCP_DIR").map_or_else(
        |_| crate::paths::product_join("pw/mcp"),
        |p| {
            let t = p.trim();
            if t.is_empty() {
                crate::paths::product_join("pw/mcp")
            } else {
                PathBuf::from(t)
            }
        },
    );
    let _ = std::fs::create_dir_all(&dir);
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

fn write_step_text(round: &Path, name: &str, body: &str) {
    let path = round.join(name);
    if let Err(e) = std::fs::write(&path, body) {
        warn!(error = %e, path = %path.display(), "tools: failed to write step artifact");
    }
}

/// Host browser binary choice: `brave` (default) or `chromium`.
#[must_use]
pub fn pw_browser_name() -> String {
    match std::env::var("ITCY_PW_BROWSER")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "chromium" | "chrome" => "chromium".into(),
        _ => "brave".into(),
    }
}

fn resolve_pw_profile_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ITCY_PW_USER_DATA_DIR") {
        let t = p.trim();
        if !t.is_empty() {
            return PathBuf::from(t);
        }
    }
    let name = if pw_browser_name() == "chromium" {
        "profile-chromium"
    } else {
        "profile-brave"
    };
    let preferred = resolve_pw_subdir(name);
    if preferred.is_dir() {
        return preferred;
    }
    let legacy = resolve_pw_subdir("profile");
    if legacy.is_dir() {
        return legacy;
    }
    preferred
}

/// Playwright often writes relative screenshots under cwd (`backend/` or product root)
/// even when `--output-dir` is set. Prefer `pw/mcp` + absolute filename from caller.
fn find_screenshot_staging(staging_name: &str, mcp_dir: &Path, mcp_msg: &str) -> Option<PathBuf> {
    let base = Path::new(staging_name).file_name().map_or_else(
        || staging_name.to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from(staging_name),
        mcp_dir.join(&base),
        PathBuf::from("backend").join(&base),
        crate::paths::product_join("backend").join(&base),
        crate::paths::product_join(&base),
        PathBuf::from(&base),
    ];
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(&base));
        if let Some(parent) = cwd.parent() {
            candidates.push(parent.join(&base));
            candidates.push(parent.join("backend").join(&base));
            candidates.push(parent.join("pw/mcp").join(&base));
            candidates.push(parent.join("sql/pw-mcp").join(&base));
        }
    }
    for token in mcp_msg.split_whitespace() {
        let t = token.trim_matches(|c| {
            c == '"' || c == '\'' || c == '`' || c == ')' || c == '(' || c == ']'
        });
        if Path::new(t)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        {
            let p = PathBuf::from(t.trim_start_matches("./"));
            candidates.push(p.clone());
            if p.is_relative() {
                candidates.push(crate::paths::product_join("backend").join(&p));
                candidates.push(crate::paths::product_join(&p));
                candidates.push(mcp_dir.join(&p));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn sweep_orphan_screenshot_names(basename: Option<&str>) {
    let Some(name) = basename.filter(|n| {
        n.starts_with("itcy-")
            && Path::new(n)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
    }) else {
        return;
    };
    for p in [
        crate::paths::product_join(name),
        crate::paths::product_join("backend").join(name),
    ] {
        if p.is_file() {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// SERP site opened in the host browser: `brave` (default) or `google`.
#[must_use]
pub fn serp_engine_name() -> String {
    match std::env::var("ITCY_SERP_ENGINE")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "google" => "google".into(),
        _ => "brave".into(),
    }
}

/// All (web) SERP tab scrape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SerpScope {
    All,
}

fn serp_search_url(serp_engine: &str, _scope: SerpScope, q: &str) -> String {
    let enc = encode_query_component(q);
    match serp_engine {
        "duckduckgo" => format!("https://duckduckgo.com/?q={enc}&ia=web"),
        "brave" => format!("https://search.brave.com/search?source=web&summary=1&q={enc}"),
        _ => format!("https://www.google.com/search?hl=en&num=10&q={enc}"),
    }
}

/// JS evaluate for SERP organic links + AI overview region.
fn serp_extract_js(serp_engine: &str) -> String {
    // Shared helpers; engine-specific root / selectors / block detection.
    let (root_expr, anchor_sel, blocked_expr, ai_extra) = match serp_engine {
        "duckduckgo" => (
            "document.querySelector('#links') || document.querySelector('[data-testid=\"react-results\"]') || document.querySelector('main') || document.body",
            "a[data-testid=\"result-title-a\"], a.result__a, article[data-testid=\"result\"] a[href], a[href]",
            "!!(/bots use duckduckgo/i.test(bodyText) || /select all squares/i.test(bodyText))",
            "",
        ),
        "brave" => (
            "document.querySelector('#results') || document.querySelector('main') || document.body",
            "a[href]",
            "!!(/verifying you.?re not a bot|pow-captcha|traditional captcha|quick check before you continue searching|unusual traffic/i.test(bodyText))",
            "\
const braveAi = document.querySelector('[data-testid=\"summarizer\"], [data-testid=\"llm-output\"], .summarizer, #infobox, .fp-card, [class*=\"ai-overview\"], [class*=\"AiOverview\"], [class*=\"answer\"], [class*=\"Summary\"], [data-testid*=\"summary\"]');\
if (braveAi) { ai = (braveAi.innerText || '').trim(); }\
if (!ai) {\
  const headers = document.querySelectorAll('h1,h2,h3,[role=\"heading\"]');\
  for (const h of headers) {\
    const ht = (h.innerText || '').toLowerCase();\
    if (ht.includes('ai overview') || ht.includes('summary') || ht.includes('answer')) {\
      const box = h.closest('section,div,article') || h.parentElement;\
      if (box) { ai = (box.innerText || '').trim(); if (ai.length > 40) break; }\
    }\
  }\
}",
        ),
        _ => (
            "document.querySelector('#search') || document.querySelector('#rso') || document.body",
            "a.zReHs, .yuRUbf a[href], a[jsname=\"UWckNb\"], a[href]",
            "!!(location.pathname.includes('/sorry') || /unusual traffic/i.test(bodyText))",
            "\
const gAi = document.querySelector('[data-attrid=\"wa:/description\"], .YzCcne, .JT2Xod, [aria-label*=\"AI Overview\"], [class*=\"ai-overview\"], [class*=\"AiOverview\"]');\
if (gAi) { ai = (gAi.innerText || '').trim(); }",
        ),
    };
    format!(
        "() => {{\
const out = [];\
const seen = new Set();\
const unwrap = (h) => {{\
  try {{\
    const u = new URL(h);\
    if (u.hostname.includes('google.') && (u.pathname === '/url' || u.searchParams.has('url'))) {{\
      const q = u.searchParams.get('q') || u.searchParams.get('url');\
      if (q && q.startsWith('http')) return q;\
    }}\
    if (u.hostname.includes('duckduckgo.com') && u.pathname.startsWith('/l')) {{\
      const uddg = u.searchParams.get('uddg');\
      if (uddg) {{ try {{ return decodeURIComponent(uddg); }} catch (e) {{ return uddg; }} }}\
    }}\
    return h;\
  }} catch (e) {{ return h; }}\
}};\
const push = (h, t) => {{\
  h = unwrap(h || '');\
  if (!h.startsWith('http')) return;\
  const low = h.toLowerCase();\
  if (low.includes('google.') || low.includes('gstatic.') || low.includes('search.brave.com') || low.includes('duckduckgo.com') || low.includes('duck.com') || low.includes('youtube.com') || low.includes('youtu.be') || low.includes('reddit.com') || low.includes('redd.it') || low.includes('example.com') || low.includes('example.org') || low.includes('raw.githubusercontent.com') || low.includes('linkedin.com') || low.includes('lnkd.in') || low.includes('instagram.com') || low.includes('facebook.com') || low.includes('tiktok.com')) return;\
  h = h.replace(/\\\\+$/g, '');\
  if (seen.has(h)) return;\
  seen.add(h);\
  out.push({{ url: h, title: (t || '').trim().replace(/\\s+/g, ' ').slice(0, 120) }});\
}};\
const root = {root_expr};\
const anchors = root.querySelectorAll('{anchor_sel}');\
for (const a of anchors) {{\
  let title = '';\
  const h3 = a.querySelector('h3');\
  if (h3 && h3.innerText) title = h3.innerText;\
  else {{\
    const card = a.closest('div') || a.parentElement;\
    const th = card && (card.querySelector('h3, .title, [class*=\"title\"]'));\
    if (th && th.innerText) title = th.innerText;\
    else title = (a.getAttribute('aria-label') || a.innerText || '').trim();\
  }}\
  if (title.length > 180) title = title.slice(0, 120);\
  push(a.href, title);\
  if (out.length >= 20) break;\
}}\
const bodyText = (document.body && document.body.innerText) || '';\
let ai = '';\
{ai_extra}\
if (!ai) {{\
  const nodes = document.querySelectorAll('div, section, article');\
  for (const n of nodes) {{\
    const label = ((n.getAttribute('aria-label') || '') + ' ' + (n.className || '')).toLowerCase();\
    if (label.includes('ai overview') || label.includes('summarizer') || label.includes('ai summary') || label.includes('answer')) {{\
      ai = (n.innerText || '').trim();\
      if (ai.length > 40) break;\
    }}\
  }}\
}}\
ai = (ai || '').replace(/\\s+/g, ' ').trim().slice(0, 4000);\
return JSON.stringify({{\
  blocked: {blocked_expr},\
  href: String(location.href || ''),\
  ai_overview: ai,\
  links: out\
}});\
}}"
    )
}

fn round_stamp() -> String {
    use chrono::Local;
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

fn write_round_meta(round: &Path, fields: &[(&str, &str)]) {
    let mut body = String::new();
    for (k, v) in fields {
        body.push_str(k);
        body.push_str(": ");
        body.push_str(v);
        body.push('\n');
    }
    let path = round.join("meta.txt");
    if let Err(e) = std::fs::write(&path, body) {
        warn!(error = %e, path = %path.display(), "tools: failed to write round meta");
    } else {
        debug!(path = %path.display(), "tools: round meta written");
    }
}

/// Strips MCP markdown / quotes and returns a bare http(s) URL when present.
fn normalize_evaluated_url(raw: &str) -> Option<String> {
    // Browse helper wraps evaluate as: ### Result\n"https://..."\n### Ran Playwright code
    for token in raw.split_whitespace() {
        let t = token.trim_matches(|c| c == '"' || c == '\'' || c == '`' || c == ',');
        if t.starts_with("http://") || t.starts_with("https://") {
            return Some(t.to_string());
        }
    }
    if let Ok(JsonValue::String(s)) = serde_json::from_str::<JsonValue>(raw.trim()) {
        if s.starts_with("http://") || s.starts_with("https://") {
            return Some(s);
        }
    }
    None
}

fn is_short_link_url(url: &str) -> bool {
    crate::sources::url_hygiene::is_shortener_url(url)
}

fn is_linkedin_login_wall(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.contains("linkedin.com/uas/login")
        || u.contains("linkedin.com/login")
        || u.contains("linkedin.com/checkpoint")
        || u.contains("linkedin.com/authwall")
        || (u.contains("linkedin.com/") && u.contains("session_redirect="))
}

/// Clear not-found / 404 markers in accessibility snapshots (not articles about HTTP 404).
const NOT_FOUND_SNAPSHOT_MARKERS: &[&str] = &[
    "page not found",
    "404 not found",
    "error 404",
    "http error 404",
    "404 - file or directory not found",
    "this page doesn't exist",
    "this page does not exist",
    "sorry, we couldn't find that page",
    "sorry, we could not find that page",
];

/// True when the accessibility snapshot is a clear not-found / 404 page (not an article about 404s).
fn snapshot_looks_like_not_found(snap: &str) -> bool {
    let lower = snap.to_ascii_lowercase();
    NOT_FOUND_SNAPSHOT_MARKERS.iter().any(|m| lower.contains(m))
}

/// Indices to close from a Playwright `browser_tabs` list (highest first). Keeps the `(current)` tab.
fn tab_indices_to_close(list_text: &str) -> Vec<usize> {
    let mut current: Option<usize> = None;
    let mut all: Vec<usize> = Vec::new();
    for line in list_text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("- ") else {
            continue;
        };
        let Some((idx_s, after)) = rest.split_once(':') else {
            continue;
        };
        let Ok(idx) = idx_s.trim().parse::<usize>() else {
            continue;
        };
        all.push(idx);
        if after.contains("(current)") {
            current = Some(idx);
        }
    }
    let Some(keep) = current else {
        return Vec::new();
    };
    let mut close: Vec<usize> = all.into_iter().filter(|&i| i != keep).collect();
    close.sort_unstable_by(|a, b| b.cmp(a));
    close.dedup();
    close
}

fn looks_like_mcp_browser_error(text: &str) -> bool {
    let t = text.trim();
    t.contains("createBrowserWithInfo")
        || t.contains("Executable doesn't exist")
        || (t.contains("### Error") && t.contains("not found"))
}

fn object_args(pairs: &[(&str, JsonValue)]) -> Option<Map<String, JsonValue>> {
    if pairs.is_empty() {
        return None;
    }
    let mut map = Map::new();
    for (k, v) in pairs {
        map.insert((*k).to_string(), v.clone());
    }
    Some(map)
}

/// Application/x-www-form-urlencoded style query encoding (ASCII-focused + UTF-8 bytes).
fn encode_query_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            b' ' => out.push('+'),
            byte => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

fn call_tool_result_to_string(result: &rmcp::model::CallToolResult) -> String {
    let mut out = String::new();
    for content in &result.content {
        if let Some(text) = content.as_text() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text.text);
        }
    }
    if out.is_empty() {
        if let Some(ref v) = result.structured_content {
            out = serde_json::to_string(v).unwrap_or_default();
        }
    }
    out
}

/// Resolves the product host-browser launch script to an **absolute** path.
/// Relative paths break when the MCP child cwd changes (e.g. pw/screenshots).
#[must_use]
pub fn resolve_host_browser_cmd() -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.push(crate::paths::product_join("scripts/playwright-mcp.sh"));
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("scripts/playwright-mcp.sh"));
        candidates.push(cwd.join("../scripts/playwright-mcp.sh"));
        if let Ok(canon) = cwd.canonicalize() {
            candidates.push(canon.join("scripts/playwright-mcp.sh"));
            if let Some(parent) = canon.parent() {
                candidates.push(parent.join("scripts/playwright-mcp.sh"));
            }
        }
    }
    candidates.push(PathBuf::from("scripts/playwright-mcp.sh"));
    candidates.push(PathBuf::from("../scripts/playwright-mcp.sh"));

    for c in candidates {
        if c.is_file() {
            return c.canonicalize().unwrap_or(c);
        }
    }
    crate::paths::product_join("scripts/playwright-mcp.sh")
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn resolve_host_browser_returns_absolute_existing_script() {
        let p = resolve_host_browser_cmd();
        assert!(
            p.is_absolute(),
            "expected absolute path, got {}",
            p.display()
        );
        assert!(p.is_file(), "playwright script missing at {}", p.display());
        assert!(
            !p.to_string_lossy().contains(".."),
            "resolved path should not keep .. segments: {}",
            p.display()
        );
    }

    #[test]
    fn pw_mcp_dir_is_absolute_under_product_tree() {
        let d = resolve_pw_mcp_dir();
        assert!(
            d.is_absolute(),
            "mcp dir must be absolute, got {}",
            d.display()
        );
        let want = crate::paths::product_join("pw/mcp");
        let _ = std::fs::create_dir_all(&want);
        let got = std::fs::canonicalize(&d).unwrap_or(d);
        let want = std::fs::canonicalize(&want).unwrap_or(want);
        assert_eq!(got, want);
        assert!(
            !got.to_string_lossy().contains("/pw/mcp/../"),
            "must not be a parent-relative dump: {}",
            got.display()
        );
    }

    #[test]
    fn absolute_script_still_exists_from_screenshots_cwd() {
        let script = resolve_host_browser_cmd();
        let shots = resolve_pw_screenshots_dir();
        let _ = std::fs::create_dir_all(&shots);
        let prev = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&shots).expect("chdir screenshots");
        let ok = script.is_file();
        let _ = std::env::set_current_dir(prev);
        assert!(
            ok,
            "absolute script must remain visible after chdir to {}",
            shots.display()
        );
    }
}

#[cfg(test)]
mod hygiene_tests {
    use super::*;

    #[test]
    fn tab_indices_to_close_keeps_current_closes_rest_high_first() {
        let list = "\
### Open tabs
- 0: (current) [Page not found | CyberInsider](https://cyberinsider.com/x)
- 1: [](about:blank)
- 3: [AI Can No Longer Rampage Through Rust's Code Repo](https://itsfoss.com/news/rust-code-repo-ai-policy/)
- 6: [InfoWorld](https://www.infoworld.com/article/x)
";
        assert_eq!(tab_indices_to_close(list), vec![6, 3, 1]);
    }

    #[test]
    fn tab_indices_to_close_empty_when_no_current() {
        let list = "### Open tabs\n- 0: [only](https://example.com)\n";
        assert!(tab_indices_to_close(list).is_empty());
    }

    #[test]
    fn tab_indices_to_close_noop_when_only_current() {
        let list = "### Open tabs\n- 0: (current) [ok](https://example.com)\n";
        assert!(tab_indices_to_close(list).is_empty());
    }

    #[test]
    fn snapshot_not_found_detects_cyberinsider_title() {
        let snap = "### Open tabs\n- 0: (current) [Page not found | CyberInsider](https://cyberinsider.com/risk-threat-index-fake-developer-job-offers)\n";
        assert!(snapshot_looks_like_not_found(snap));
    }

    #[test]
    fn snapshot_not_found_rejects_normal_article() {
        let snap = "Heading\nRisk and Threat Index covers fake developer job offers.\n";
        assert!(!snapshot_looks_like_not_found(snap));
    }
}

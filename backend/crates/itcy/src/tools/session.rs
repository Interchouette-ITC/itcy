// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! One research session folder + story.txt for a load→draft (or freeform) tool run.

use crate::llm::prompt_dump::set_session_prompt_dir;
use crate::logging::{append_session_log_note, attach_session_log, detach_session_log};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use tracing::{info, warn};

/// Policy for tool calls during a phase.
#[derive(Debug, Clone)]
pub struct ToolPolicy {
    /// When false, `web_search` is refused (draft with pack URLs).
    pub allow_web_search: bool,
    /// After a search that returned EXTRACTED links, require `browse_url` before another search.
    pub require_browse_before_research: bool,
    /// When non-empty, `browse_url` may open only these publisher URLs (draft writer lock).
    pub pack_url_allowlist: Vec<String>,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            allow_web_search: true,
            require_browse_before_research: true,
            pack_url_allowlist: Vec::new(),
        }
    }
}

/// Writer policy after LOAD: pack URLs lock the cite; empty pack may search.
#[must_use]
pub fn draft_writer_policy(pack_urls: &[String]) -> ToolPolicy {
    let pack_has = !pack_urls.is_empty();
    ToolPolicy {
        allow_web_search: !pack_has,
        require_browse_before_research: false,
        pack_url_allowlist: pack_urls.to_vec(),
    }
}

/// Live research session under `pw/screenshots/<Draft-ID>/`.
pub struct ResearchSession {
    pub root: PathBuf,
    /// Operator reference (`DRAFT-YYYYMMDD-NNNNNN`); also the folder name.
    pub draft_id: String,
    step: AtomicU32,
    web_searches: AtomicU32,
    browses: AtomicU32,
    /// True when the last `web_search` returned at least one EXTRACTED publisher link.
    last_search_had_extracted: std::sync::atomic::AtomicBool,
    /// Successful browse `final_urls` (for pack recovery when the model forgets to list them).
    browsed_urls: Mutex<Vec<String>>,
    /// Publisher URLs from the latest `web_search` EXTRACTED block.
    extracted_urls: Mutex<Vec<String>>,
    /// Page text from successful `browse_url` (fed into the writer pack).
    browse_excerpts: Mutex<Vec<(String, String)>>,
}

impl ResearchSession {
    /// Creates `pw/screenshots/<draft_id>/` + `story.txt` + tees `product.log`.
    /// Also updates `pw/screenshots/latest` → this folder.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] when the filesystem operation fails.
    pub fn start(screenshots_root: &Path, subject: &str, draft_id: &str) -> std::io::Result<Self> {
        let id = draft_id.trim();
        if id.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty draft_id",
            ));
        }
        let root = screenshots_root.join(id);
        std::fs::create_dir_all(&root)?;
        let _ = std::fs::write(root.join("DRAFT_ID.txt"), format!("{id}\n"));
        // Easy pointer for humans browsing pw/screenshots/
        let latest = screenshots_root.join("latest");
        let _ = std::fs::remove_file(&latest);
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let _ = symlink(id, &latest);
        }
        let product_log = root.join("product.log");
        attach_session_log(&product_log)?;
        set_session_prompt_dir(Some(root.clone()));
        append_session_log_note(&format!(
            "======== ITCy research session log ========\n\
started: {}\n\
draft_id: {id}\n\
subject: {}\n\
dir: {}\n\
note: full product tracing teed here until session ends (plain text, no ANSI).\n\
============================================\n",
            chrono::Local::now().to_rfc3339(),
            subject.chars().take(200).collect::<String>(),
            root.display()
        ));
        let story = root.join("story.txt");
        let header = format!(
            "ITCy research session\n\
draft_id: {id}\n\
started: {}\n\
subject: {}\n\
note: one folder for LOAD + DRAFT. See product.log for full tracing. \
Each step has its own subdir + screenshot. web_search writes ai_overview_*.txt + extracted_*.txt.\n\
---\n",
            chrono::Local::now().to_rfc3339(),
            subject.chars().take(200).collect::<String>()
        );
        std::fs::write(&story, header)?;
        info!(dir = %root.display(), draft_id = %id, "tools: research session started");
        Ok(Self {
            root,
            draft_id: id.to_string(),
            step: AtomicU32::new(0),
            web_searches: AtomicU32::new(0),
            browses: AtomicU32::new(0),
            last_search_had_extracted: std::sync::atomic::AtomicBool::new(false),
            browsed_urls: Mutex::new(Vec::new()),
            extracted_urls: Mutex::new(Vec::new()),
            browse_excerpts: Mutex::new(Vec::new()),
        })
    }

    pub fn story_path(&self) -> PathBuf {
        self.root.join("story.txt")
    }

    pub fn append_story(&self, block: &str) {
        use std::io::Write;
        let path = self.story_path();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{block}");
        } else {
            warn!(path = %path.display(), "tools: could not append story.txt");
        }
    }

    /// Next step dir: `01-web_search`, `02-browse`, …
    pub fn next_step_dir(&self, kind: &str) -> PathBuf {
        let n = self.step.fetch_add(1, Ordering::SeqCst) + 1;
        let dir = self.root.join(format!("{n:02}-{kind}"));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    pub fn record_web_search(&self, had_extracted: bool) {
        self.web_searches.fetch_add(1, Ordering::SeqCst);
        self.last_search_had_extracted
            .store(had_extracted, Ordering::SeqCst);
    }

    pub fn record_extracted_urls(&self, urls: &[String]) {
        if let Ok(mut g) = self.extracted_urls.lock() {
            for u in urls {
                let u = u.trim();
                if (u.starts_with("http://") || u.starts_with("https://"))
                    && !g.iter().any(|x| x == u)
                {
                    g.push(u.to_string());
                }
            }
        }
    }

    pub fn extracted_urls(&self) -> Vec<String> {
        self.extracted_urls
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    pub fn record_browse(&self, final_url: Option<&str>) {
        self.browses.fetch_add(1, Ordering::SeqCst);
        self.last_search_had_extracted
            .store(false, Ordering::SeqCst);
        if let Some(u) = final_url {
            let u = u.trim();
            if u.starts_with("http://") || u.starts_with("https://") {
                if let Ok(mut g) = self.browsed_urls.lock() {
                    if !g.iter().any(|x| x == u) {
                        g.push(u.to_string());
                    }
                }
            }
        }
    }

    pub fn browsed_urls(&self) -> Vec<String> {
        self.browsed_urls
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    pub fn record_browse_excerpt(&self, url: &str, text: &str) {
        let url = url.trim();
        let text = text.trim();
        if url.is_empty() || text.is_empty() {
            return;
        }
        if let Ok(mut g) = self.browse_excerpts.lock() {
            if g.iter().any(|(u, _)| u == url) {
                return;
            }
            g.push((url.to_string(), text.to_string()));
        }
    }

    pub fn browse_excerpts(&self) -> Vec<(String, String)> {
        self.browse_excerpts
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    pub fn web_search_count(&self) -> u32 {
        self.web_searches.load(Ordering::SeqCst)
    }

    pub fn browse_count(&self) -> u32 {
        self.browses.load(Ordering::SeqCst)
    }

    pub fn last_search_had_extracted(&self) -> bool {
        self.last_search_had_extracted.load(Ordering::SeqCst)
    }

    /// Clear EXTRACTED gate (e.g. load produced no publisher URLs; draft may search fresh).
    pub fn clear_extracted_gate(&self) {
        self.last_search_had_extracted
            .store(false, Ordering::SeqCst);
    }

    pub fn finish(&self, note: &str) {
        self.append_story(&format!(
            "---\nended: {}\nweb_searches: {}\nbrowses: {}\nbrowsed_urls: {}\n{note}\n",
            chrono::Local::now().to_rfc3339(),
            self.web_search_count(),
            self.browse_count(),
            self.browsed_urls().join(" | ")
        ));
        info!(
            dir = %self.root.display(),
            web_searches = self.web_search_count(),
            browses = self.browse_count(),
            "tools: research session ended"
        );
        append_session_log_note(&format!(
            "\n======== session ended {} ========\n",
            chrono::Local::now().to_rfc3339()
        ));
        set_session_prompt_dir(None);
        detach_session_log();
    }
}

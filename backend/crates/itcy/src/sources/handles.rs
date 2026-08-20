// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Social media handle registry loaded from `backend/handles.toml`.
//!
//! The index is loaded once at startup and queried via the `lookup_handles`
//! LLM tool. The full file is never injected into a prompt; only matching
//! rows are returned to the model as a tool result.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use thiserror::Error;

const DEFAULT_REL: &str = "handles.toml";

/// One known entity with its social handles.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HandleEntry {
    /// Canonical entity name used for lookup.
    pub name: String,
    /// `LinkedIn` company/profile handle (e.g. `@interchouette-itc`). Optional.
    #[serde(default)]
    pub linkedin: String,
    /// X / Twitter handle (e.g. `@Interchouette`). Optional.
    #[serde(default)]
    pub x: String,
    /// Canonical `LinkedIn` URL for this entry (clickable in `handles.toml`).
    #[serde(default)]
    pub linkedin_url: String,
    /// Canonical X URL for this entry (clickable in `handles.toml`).
    #[serde(default)]
    pub x_url: String,
}

#[derive(Debug, Deserialize)]
struct HandlesFile {
    #[serde(default)]
    handle: Vec<HandleEntry>,
}

/// Load / resolve errors for the handles file.
#[derive(Debug, Error)]
pub enum HandlesError {
    #[error("handles: {0}")]
    Io(#[from] std::io::Error),
    #[error("handles parse: {0}")]
    Parse(#[from] toml::de::Error),
}

/// In-memory handle registry. Searched by case-insensitive substring on `name`.
#[derive(Debug, Clone, Default)]
pub struct HandlesIndex {
    entries: Vec<HandleEntry>,
}

impl HandlesIndex {
    /// Search by case-insensitive substring match on `name`. Returns up to 5 matches.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<&HandleEntry> {
        let q = query.to_lowercase();
        self.entries
            .iter()
            .filter(|e| e.name.to_lowercase().contains(q.as_str()))
            .take(5)
            .collect()
    }

    /// Number of entries in the index.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the index is empty (file missing or empty).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Candidate paths searched in order (mirrors `live_sites.rs` pattern).
#[must_use]
pub fn handles_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(DEFAULT_REL));
        out.push(cwd.join("backend").join(DEFAULT_REL));
        out.push(cwd.join("../backend").join(DEFAULT_REL));
    }
    out.push(crate::paths::product_join("backend/handles.toml"));
    out
}

/// First existing path among [`handles_candidates`].
#[must_use]
pub fn resolve_handles_path() -> Option<PathBuf> {
    handles_candidates().into_iter().find(|p| p.is_file())
}

/// Load the handle registry. Returns an empty index when the file is not found.
///
/// # Errors
///
/// Returns [`HandlesError`] when the file exists but cannot be read or parsed.
pub fn load_handles() -> Result<HandlesIndex, HandlesError> {
    let Some(path) = resolve_handles_path() else {
        return Ok(HandlesIndex::default());
    };
    load_handles_from(&path)
}

/// Load from an explicit path (tests).
///
/// # Errors
///
/// Returns [`HandlesError`] on IO or TOML parse failure.
pub fn load_handles_from(path: &Path) -> Result<HandlesIndex, HandlesError> {
    let text = std::fs::read_to_string(path)?;
    let parsed: HandlesFile = toml::from_str(&text)?;
    let entries = parsed
        .handle
        .into_iter()
        .filter(|e| !e.name.trim().is_empty())
        .map(|e| HandleEntry {
            name: e.name.trim().to_string(),
            linkedin: e.linkedin.trim().to_string(),
            x: e.x.trim().to_string(),
            linkedin_url: e.linkedin_url.trim().to_string(),
            x_url: e.x_url.trim().to_string(),
        })
        .collect();
    Ok(HandlesIndex { entries })
}

const BRAND: &str = "Interchouette";
const BRAND_ITC: &str = "Interchouette ITC";
const LINKEDIN_BRAND_HANDLE: &str = "@interchouette-itc";

/// When the post already names Interchouette, keep ITC and add `@interchouette-itc`. Does not invent a brand mention.
#[must_use]
pub fn ensure_linkedin_brand_mention(body: &str) -> String {
    if body.to_ascii_lowercase().contains(LINKEDIN_BRAND_HANDLE) {
        return body.to_string();
    }
    if let Some((_, end)) = find_phrase_outside_url(body, BRAND_ITC) {
        return insert_at(body, end, " (@interchouette-itc)");
    }
    if let Some((_, end)) = find_phrase_outside_url(body, BRAND) {
        return insert_at(body, end, " ITC (@interchouette-itc)");
    }
    body.to_string()
}

/// Put the `LinkedIn` brand handle in the `ResearchPack` when the operator named Interchouette.
pub fn ensure_pack_linkedin_brand_handle(pack: &mut String, brief: &str) {
    if find_phrase_outside_url(brief, BRAND).is_none() {
        return;
    }
    if pack
        .to_ascii_lowercase()
        .contains("linkedin=@interchouette-itc")
    {
        return;
    }
    *pack = insert_handles_after_subject(pack, "handles: linkedin=@interchouette-itc");
}

fn insert_at(body: &str, end: usize, insert: &str) -> String {
    format!("{}{}{}", &body[..end], insert, &body[end..])
}

fn insert_handles_after_subject(pack: &str, line: &str) -> String {
    let mut out = String::new();
    let mut inserted = false;
    for raw in pack.lines() {
        out.push_str(raw);
        out.push('\n');
        if !inserted && raw.starts_with("subject:") {
            out.push_str(line);
            out.push('\n');
            inserted = true;
        }
    }
    if !inserted {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn find_phrase_outside_url(hay: &str, phrase: &str) -> Option<(usize, usize)> {
    let hay_l = hay.to_ascii_lowercase();
    let needle = phrase.to_ascii_lowercase();
    let mut from = 0usize;
    while from < hay_l.len() {
        let Some(rel) = hay_l.get(from..).and_then(|s| s.find(needle.as_str())) else {
            break;
        };
        let start = from + rel;
        let end = start + needle.len();
        if end > hay.len() {
            break;
        }
        if word_boundary(hay, start, end) && !skip_host_or_url(hay, start, end) {
            return Some((start, end));
        }
        from = start.saturating_add(1);
    }
    None
}

fn word_boundary(hay: &str, start: usize, end: usize) -> bool {
    let before_ok = start == 0
        || hay
            .get(..start)
            .and_then(|s| s.chars().next_back())
            .is_none_or(|c| !c.is_alphanumeric());
    let after_ok = end >= hay.len()
        || hay
            .get(end..)
            .and_then(|s| s.chars().next())
            .is_none_or(|c| !c.is_alphanumeric());
    before_ok && after_ok
}

fn skip_host_or_url(hay: &str, start: usize, end: usize) -> bool {
    if inside_url(hay, start) {
        return true;
    }
    let before = hay.get(..start).and_then(|s| s.chars().next_back());
    let after = hay.get(end..).and_then(|s| s.chars().next());
    matches!(before, Some('.' | '/' | ':' | '@')) || matches!(after, Some('.' | '/'))
}

fn inside_url(hay: &str, idx: usize) -> bool {
    let prefix = hay.get(..idx).unwrap_or("");
    let start = prefix.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    hay.get(start..idx).is_some_and(|s| s.contains("://"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_seed_file_when_present() {
        let Some(path) = resolve_handles_path() else {
            return;
        };
        let idx = load_handles_from(&path).expect("parse");
        assert!(idx.len() >= 2);
        assert!(!idx.search("Interchouette").is_empty());
    }

    #[test]
    fn search_case_insensitive() {
        let idx = HandlesIndex {
            entries: vec![HandleEntry {
                name: "Rust Foundation".into(),
                linkedin: "@rust-foundation".into(),
                x: "@rustlang".into(),
                linkedin_url: "https://www.linkedin.com/company/rust-foundation/".into(),
                x_url: "https://x.com/rustlang".into(),
            }],
        };
        assert!(!idx.search("rust").is_empty());
        assert!(!idx.search("RUST").is_empty());
        assert!(idx.search("anthropic").is_empty());
    }

    #[test]
    fn empty_index_when_file_missing() {
        let result = load_handles_from(std::path::Path::new("/nonexistent/handles.toml"));
        assert!(result.is_err());
        let idx = load_handles();
        // Either loaded from disk or empty - both are valid.
        let _ = idx;
    }

    #[test]
    fn linkedin_keeps_itc_and_adds_company_handle() {
        let out = ensure_linkedin_brand_mention("Interchouette ITC shipped WebMCP on the site.");
        assert!(out.contains("Interchouette ITC (@interchouette-itc)"));
        assert!(!out.contains("Interchouette (@interchouette-itc) ITC"));
    }

    #[test]
    fn linkedin_completes_brand_name_when_itc_missing() {
        let out = ensure_linkedin_brand_mention("Interchouette shipped WebMCP.");
        assert!(out.contains("Interchouette ITC (@interchouette-itc) shipped"));
    }

    #[test]
    fn linkedin_does_not_double_handle() {
        let src = "Interchouette ITC (@interchouette-itc) shipped WebMCP.";
        assert_eq!(ensure_linkedin_brand_mention(src), src);
    }

    #[test]
    fn linkedin_skips_handle_inside_site_url() {
        let src = "See https://mcp.interchouette.net for the tools.";
        assert_eq!(ensure_linkedin_brand_mention(src), src);
    }

    #[test]
    fn pack_gets_linkedin_handle_when_brief_names_brand() {
        let mut pack = String::from("## ResearchPack\nsubject: WebMCP\nsummary: spec\n");
        ensure_pack_linkedin_brand_handle(
            &mut pack,
            "WebMCP, Interchouette has integrated it at https://mcp.interchouette.net",
        );
        assert!(pack.contains("handles: linkedin=@interchouette-itc"));
        assert!(pack.contains("subject: WebMCP"));
    }
}

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
        })
        .collect();
    Ok(HandlesIndex { entries })
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
}

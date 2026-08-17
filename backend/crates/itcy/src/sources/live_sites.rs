// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Authorized live hub list for daily digest (committed TOML).

use serde::Deserialize;
use std::path::{Path, PathBuf};
use thiserror::Error;

const DEFAULT_REL: &str = "live_sites.toml";

/// One authorized hub the digest may parse.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LiveSite {
    pub url: String,
    #[serde(default = "default_weight")]
    pub weight: i32,
    #[serde(default)]
    pub note: String,
}

const fn default_weight() -> i32 {
    10
}

#[derive(Debug, Deserialize)]
struct LiveSitesFile {
    #[serde(default)]
    site: Vec<LiveSite>,
}

/// Load / resolve errors for the live-sites file.
#[derive(Debug, Error)]
pub enum LiveSitesError {
    #[error("live_sites: {0}")]
    Io(#[from] std::io::Error),
    #[error("live_sites parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("live_sites: {0}")]
    Other(String),
}

/// Candidate paths (cwd when `make run` is `backend/`, then product root).
#[must_use]
pub fn live_sites_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(DEFAULT_REL));
        out.push(cwd.join("backend").join(DEFAULT_REL));
        out.push(cwd.join("../backend").join(DEFAULT_REL));
    }
    out.push(crate::paths::product_join("backend/live_sites.toml"));
    out
}

/// First existing path among [`live_sites_candidates`].
#[must_use]
pub fn resolve_live_sites_path() -> Option<PathBuf> {
    live_sites_candidates().into_iter().find(|p| p.is_file())
}

/// Loads authorized hubs. Empty file → empty vec (digest continues with other lanes).
///
/// # Errors
///
/// Returns [`LiveSitesError`] when the file exists but cannot be read or parsed.
pub fn load_live_sites() -> Result<Vec<LiveSite>, LiveSitesError> {
    let Some(path) = resolve_live_sites_path() else {
        return Err(LiveSitesError::Other(
            "backend/live_sites.toml not found (authorized hubs)".into(),
        ));
    };
    load_live_sites_from(&path)
}

/// Loads hubs from an explicit path (tests).
///
/// # Errors
///
/// Returns [`LiveSitesError`] on IO or TOML parse failure.
pub fn load_live_sites_from(path: &Path) -> Result<Vec<LiveSite>, LiveSitesError> {
    let text = std::fs::read_to_string(path)?;
    let parsed: LiveSitesFile = toml::from_str(&text)?;
    let mut sites = Vec::new();
    for s in parsed.site {
        let url = s.url.trim().to_string();
        if url.is_empty() {
            continue;
        }
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(LiveSitesError::Other(format!(
                "hub must be http(s): `{url}`"
            )));
        }
        sites.push(LiveSite {
            url,
            weight: s.weight,
            note: s.note.trim().to_string(),
        });
    }
    Ok(sites)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_seed_file_when_present() {
        let Some(path) = resolve_live_sites_path() else {
            return;
        };
        let sites = load_live_sites_from(&path).expect("parse");
        assert!(sites.len() >= 2);
        assert!(sites.iter().any(|s| s.url.contains("infoworld")));
        assert!(sites.iter().any(|s| s.weight == 10));
    }

    #[test]
    fn rejects_non_http() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("live_sites.toml");
        std::fs::write(&path, "[[site]]\nurl = \"ftp://x\"\nweight = 1\n").unwrap();
        assert!(load_live_sites_from(&path).is_err());
    }
}

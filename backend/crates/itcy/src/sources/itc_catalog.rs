// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Interchouette product catalog from the sibling `itc-cursor` README table.

use crate::paths::product_root;
use std::path::{Path, PathBuf};

const SITE_HOME: &str = "https://interchouette.net/";
const SITE_CV: &str = "https://interchouette.net/CV/";
const SITE_NEWS: &str = "https://interchouette.net/news";
const WORKER_PROFILE: &str = "https://github.com/Interchouette";
const ORG_PROFILE: &str = "https://github.com/Interchouette-ITC";

/// One row from the itc-cursor product table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItcCatalogEntry {
    pub branch: String,
    pub product_root: String,
}

/// Public cite target for a catalog entry (never a private product URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItcPublicCite {
    pub owner: String,
    pub repo: String,
    pub html_url: String,
}

/// Resolves the itc-cursor checkout: `ITCY_ITC_CURSOR`, else sibling `../itc-cursor`.
#[must_use]
pub fn itc_cursor_root() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("ITCY_ITC_CURSOR") {
        let t = raw.trim();
        if !t.is_empty() {
            let p = PathBuf::from(t);
            if p.join("README.md").is_file() {
                return Some(p);
            }
        }
    }
    let sibling = product_root().join("..").join("itc-cursor");
    if sibling.join("README.md").is_file() {
        return Some(sibling);
    }
    None
}

/// Loads catalog entries from `README.md` under [`itc_cursor_root`].
///
/// # Errors
///
/// Returns an error when the catalog file is missing or unreadable.
pub fn load_itc_catalog() -> Result<Vec<ItcCatalogEntry>, String> {
    let root = itc_cursor_root().ok_or_else(|| {
        "itc-cursor catalog not found (set ITCY_ITC_CURSOR or place a sibling itc-cursor checkout)"
            .to_string()
    })?;
    let text = std::fs::read_to_string(root.join("README.md"))
        .map_err(|e| format!("read itc-cursor README: {e}"))?;
    Ok(parse_product_table(&text))
}

/// Parses the Day-to-day product repos markdown table.
#[must_use]
pub fn parse_product_table(markdown: &str) -> Vec<ItcCatalogEntry> {
    let mut out = Vec::new();
    let mut in_table = false;
    for line in markdown.lines() {
        let t = line.trim();
        if !in_table {
            if t.starts_with('|') && t.to_ascii_lowercase().contains("branch") {
                in_table = true;
            }
            continue;
        }
        if !t.starts_with('|') {
            break;
        }
        if t.contains("---") {
            continue;
        }
        let cols: Vec<&str> = t
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .collect();
        if cols.len() < 2 {
            continue;
        }
        let branch = strip_ticks(cols[0]);
        let product_root = strip_ticks(cols[1]);
        if branch.eq_ignore_ascii_case("branch") {
            continue;
        }
        if branch.is_empty() {
            continue;
        }
        out.push(ItcCatalogEntry {
            branch,
            product_root,
        });
    }
    out
}

fn strip_ticks(s: &str) -> String {
    s.trim().trim_matches('`').trim().to_string()
}

/// Ops / profile rows: grounding only, not digest project items.
#[must_use]
pub fn is_digest_skip(branch: &str) -> bool {
    matches!(
        branch,
        "itc-cursor"
            | "itc-hooks"
            | "itc-ga"
            | "itcy-publications"
            | "Interchouette-gh-profile"
            | "Interchouette-ITC-gh-profile"
    )
}

/// Catalog rows that may become INTERCHOUETTE digest items.
#[must_use]
pub fn digest_eligible(entries: &[ItcCatalogEntry]) -> Vec<&ItcCatalogEntry> {
    entries
        .iter()
        .filter(|e| !is_digest_skip(&e.branch))
        .collect()
}

/// Maps a catalog branch to the GitHub API owner/repo used for activity probes.
#[must_use]
pub fn github_probe_target(branch: &str) -> Option<(String, String)> {
    match branch {
        "Interchouette-gh-profile" => Some(("Interchouette".into(), "Interchouette".into())),
        "Interchouette-ITC-gh-profile" => Some(("Interchouette-ITC".into(), ".github".into())),
        // Private product: probe public TUI as activity proxy.
        "itcy" => Some(("Interchouette-ITC".into(), "itcy-tui".into())),
        other if other.starts_with("itc-") => None,
        other => Some(("Interchouette-ITC".into(), other.to_string())),
    }
}

/// Public cite URL for posts (never the private `itcy` product repo).
#[must_use]
pub fn public_cite_for(branch: &str) -> ItcPublicCite {
    match branch {
        "itcy" | "Interchouette-gh-profile" => ItcPublicCite {
            owner: "Interchouette".into(),
            repo: "Interchouette".into(),
            html_url: WORKER_PROFILE.into(),
        },
        "Interchouette-ITC-gh-profile" => ItcPublicCite {
            owner: "Interchouette-ITC".into(),
            repo: ".github".into(),
            html_url: ORG_PROFILE.into(),
        },
        "interchouette" => ItcPublicCite {
            owner: "Interchouette-ITC".into(),
            repo: "interchouette".into(),
            html_url: SITE_HOME.into(),
        },
        other => {
            let url = format!("https://github.com/Interchouette-ITC/{other}");
            ItcPublicCite {
                owner: "Interchouette-ITC".into(),
                repo: other.to_string(),
                html_url: url,
            }
        }
    }
}

/// Site home URL.
#[must_use]
pub const fn site_home() -> &'static str {
    SITE_HOME
}

/// Founder CV URL.
#[must_use]
pub const fn site_cv() -> &'static str {
    SITE_CV
}

/// News path (cite when live).
#[must_use]
pub const fn site_news() -> &'static str {
    SITE_NEWS
}

/// Reads README from an explicit path (tests).
#[must_use]
pub fn parse_product_table_file(path: &Path) -> Option<Vec<ItcCatalogEntry>> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(parse_product_table(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r"
## Day-to-day (product repos)

| Branch | Product root |
| --- | --- |
| `alpha-demo` | alpha-demo |
| `itc-hooks` | itc-hooks |
| `itcy` | itcy |
| `itcy-tui` | itcy/tui |
| `itcy-publications` | itcy/publications |
| `Interchouette-gh-profile` | Interchouette-gh-profile |
| `widgets` | widgets |

## Commits
";

    #[test]
    fn parses_fixture_table() {
        let rows = parse_product_table(FIXTURE);
        assert_eq!(rows.len(), 7);
        assert_eq!(rows[0].branch, "alpha-demo");
        assert_eq!(rows[2].branch, "itcy");
    }

    #[test]
    fn digest_skip_filters_ops() {
        let rows = parse_product_table(FIXTURE);
        let eligible = digest_eligible(&rows);
        let names: Vec<_> = eligible.iter().map(|e| e.branch.as_str()).collect();
        assert_eq!(names, vec!["alpha-demo", "itcy", "itcy-tui", "widgets"]);
    }

    #[test]
    fn itcy_public_cite_is_not_private_repo() {
        let cite = public_cite_for("itcy");
        assert!(!cite.html_url.contains("/itcy"));
        assert!(cite.html_url.contains("Interchouette"));
    }

    #[test]
    fn github_probe_itcy_uses_tui() {
        let (o, r) = github_probe_target("itcy").unwrap();
        assert_eq!(o, "Interchouette-ITC");
        assert_eq!(r, "itcy-tui");
    }
}

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Twitter search query pool for daily digest (committed TOML).
//!
//! Large pools are fine: [`plan_twitter_searches_from_pool`] / [`plan_twitter_searches_with_excludes`]
//! pick up to [`MAX_SEARCHES_PER_RUN`] individual searches with even spacing across the
//! ranked pool (day ordinal rotates daily). No OR-batching. Rust-ish queries get
//! shared `exclude` terms (gaming noise) appended as `-term`.

use chrono::{Datelike, Local};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use thiserror::Error;

const DEFAULT_REL: &str = "twitter_queries.toml";
/// Max individual X searches per digest run.
pub const MAX_SEARCHES_PER_RUN: usize = 20;

/// One search string the digest may run via `TwitterTool`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TwitterQuery {
    pub q: String,
    #[serde(default = "default_weight")]
    pub weight: i32,
}

const fn default_weight() -> i32 {
    8
}

#[derive(Debug, Deserialize)]
struct TwitterQueriesFile {
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    query: Vec<TwitterQuery>,
}

/// Load / resolve errors for the Twitter query pool.
#[derive(Debug, Error)]
pub enum TwitterQueriesError {
    #[error("twitter_queries: {0}")]
    Io(#[from] std::io::Error),
    #[error("twitter_queries parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("twitter_queries: {0}")]
    Other(String),
}

/// Candidate paths (cwd when `make run` is `backend/`, then product root).
#[must_use]
pub fn twitter_queries_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(DEFAULT_REL));
        out.push(cwd.join("backend").join(DEFAULT_REL));
        out.push(cwd.join("../backend").join(DEFAULT_REL));
    }
    out.push(crate::paths::product_join("backend/twitter_queries.toml"));
    out
}

/// First existing path among [`twitter_queries_candidates`].
#[must_use]
pub fn resolve_twitter_queries_path() -> Option<PathBuf> {
    twitter_queries_candidates()
        .into_iter()
        .find(|p| p.is_file())
}

/// Loaded pool plus optional shared excludes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TwitterQueryPool {
    pub queries: Vec<TwitterQuery>,
    pub excludes: Vec<String>,
}

/// Loads queries + excludes from disk (or defaults).
///
/// # Errors
///
/// Returns [`TwitterQueriesError`] when the file exists but cannot be read or parsed.
pub fn load_twitter_query_pool() -> Result<TwitterQueryPool, TwitterQueriesError> {
    let Some(path) = resolve_twitter_queries_path() else {
        return Ok(TwitterQueryPool {
            queries: default_twitter_queries(),
            excludes: default_excludes(),
        });
    };
    load_twitter_query_pool_from(&path)
}

/// Loads queries + excludes from an explicit path (tests).
///
/// # Errors
///
/// Returns [`TwitterQueriesError`] on IO or TOML parse failure.
pub fn load_twitter_query_pool_from(path: &Path) -> Result<TwitterQueryPool, TwitterQueriesError> {
    let text = std::fs::read_to_string(path)?;
    let file: TwitterQueriesFile = toml::from_str(&text)?;
    let excludes: Vec<String> = file
        .exclude
        .into_iter()
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .collect();
    let mut out = Vec::new();
    for q in file.query {
        let t = q.q.trim();
        if t.is_empty() {
            continue;
        }
        out.push(TwitterQuery {
            q: t.to_string(),
            weight: q.weight,
        });
    }
    if out.is_empty() {
        return Ok(TwitterQueryPool {
            queries: default_twitter_queries(),
            excludes: if excludes.is_empty() {
                default_excludes()
            } else {
                excludes
            },
        });
    }
    Ok(TwitterQueryPool {
        queries: out,
        excludes: if excludes.is_empty() {
            default_excludes()
        } else {
            excludes
        },
    })
}

/// One planned X search (query text after excludes + weight).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSearch {
    pub q: String,
    pub weight: i32,
}

/// Daily plan: up to [`MAX_SEARCHES_PER_RUN`] individual searches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitterSearchPlan {
    /// Individual X searches for this run (no OR packing).
    pub searches: Vec<PlannedSearch>,
}

/// Plan from a loaded [`TwitterQueryPool`].
#[must_use]
pub fn plan_twitter_searches_from_pool(pool: &TwitterQueryPool) -> TwitterSearchPlan {
    plan_twitter_searches_with_excludes(
        &pool.queries,
        &pool.excludes,
        Local::now().ordinal() as usize,
    )
}

/// Same as [`plan_twitter_searches_with_excludes`] with default excludes and an explicit day ordinal (tests).
#[must_use]
pub fn plan_twitter_searches_at(pool: &[TwitterQuery], day_ordinal: usize) -> TwitterSearchPlan {
    plan_twitter_searches_with_excludes(pool, &default_excludes(), day_ordinal)
}

/// Build a plan with explicit excludes and day phase.
#[must_use]
pub fn plan_twitter_searches_with_excludes(
    pool: &[TwitterQuery],
    excludes: &[String],
    day_ordinal: usize,
) -> TwitterSearchPlan {
    let mut ranked = pool.to_vec();
    ranked.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.q.cmp(&b.q)));
    let selected = spaced_sample(&ranked, MAX_SEARCHES_PER_RUN, day_ordinal);
    TwitterSearchPlan {
        searches: selected
            .into_iter()
            .map(|q| PlannedSearch {
                q: apply_excludes_if_rustish(&q.q, excludes),
                weight: q.weight,
            })
            .collect(),
    }
}

/// Append `-term` excludes to rust-ish queries (idempotent).
#[must_use]
pub fn apply_excludes_if_rustish(query: &str, excludes: &[String]) -> String {
    let q = query.trim();
    if q.is_empty() || excludes.is_empty() || !is_rustish_query(q) {
        return q.to_string();
    }
    let mut out = q.to_string();
    let lower = out.to_ascii_lowercase();
    for raw in excludes {
        let term = raw.trim().trim_start_matches('-');
        if term.is_empty() {
            continue;
        }
        let minus = format!("-{term}");
        if lower.contains(&minus.to_ascii_lowercase())
            || lower.contains(&format!(" -{term}").to_ascii_lowercase())
        {
            continue;
        }
        out.push(' ');
        out.push_str(&minus);
    }
    out
}

fn is_rustish_query(q: &str) -> bool {
    let l = q.to_ascii_lowercase();
    l.contains("rust") || l.contains("ratatui") || l.contains("#rust")
}

fn default_excludes() -> Vec<String> {
    vec!["RustGame".into(), "RustClips".into(), "GamingYT".into()]
}

/// Positive search surface for logs (drops `-excludes` and `lang:en`).
#[must_use]
pub fn query_for_log(q: &str) -> String {
    q.split_whitespace()
        .filter(|t| {
            let t = *t;
            !t.is_empty() && !t.starts_with('-') && !t.eq_ignore_ascii_case("lang:en")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Evenly space `max` picks across `pool`; `day_ordinal` rotates the phase so a
/// large pool gets fair coverage over days (not always the top weights only).
fn spaced_sample(pool: &[TwitterQuery], max: usize, day_ordinal: usize) -> Vec<TwitterQuery> {
    if pool.is_empty() || max == 0 {
        return Vec::new();
    }
    if pool.len() <= max {
        return pool.to_vec();
    }
    let n = pool.len();
    let mut out = Vec::with_capacity(max);
    let mut seen = HashSet::new();
    for i in 0..max {
        let idx = (day_ordinal.wrapping_add((i * n) / max)) % n;
        if seen.insert(idx) {
            out.push(pool[idx].clone());
        }
    }
    let mut j = 0;
    while out.len() < max && j < n {
        let idx = (day_ordinal.wrapping_add(j)) % n;
        if seen.insert(idx) {
            out.push(pool[idx].clone());
        }
        j += 1;
    }
    out
}

fn default_twitter_queries() -> Vec<TwitterQuery> {
    vec![
        TwitterQuery {
            q: "#rustlang".into(),
            weight: 8,
        },
        TwitterQuery {
            q: "casper blockchain".into(),
            weight: 9,
        },
        TwitterQuery {
            q: "casper x402".into(),
            weight: 10,
        },
        TwitterQuery {
            q: "open source rust".into(),
            weight: 7,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_seed_file_when_present() {
        let Some(path) = resolve_twitter_queries_path() else {
            return;
        };
        let pool = load_twitter_query_pool_from(&path).expect("parse");
        assert!(pool.queries.iter().any(|q| q.q.contains("rustlang")));
        assert!(pool.queries.iter().any(|q| q.q.contains("x402")));
        assert!(
            pool.queries.len() >= 20,
            "keyword pool must be at least 20 (max {MAX_SEARCHES_PER_RUN} searches/run)"
        );
        assert!(pool.excludes.iter().any(|e| e.contains("RustGame")));
    }

    #[test]
    fn small_pool_runs_all() {
        let pool = default_twitter_queries();
        let plan = plan_twitter_searches_at(&pool, 1);
        assert_eq!(plan.searches.len(), pool.len());
        assert!(!plan.searches.iter().any(|s| s.q.contains(" OR ")));
    }

    #[test]
    fn spaced_sample_caps_and_rotates() {
        let pool: Vec<TwitterQuery> = (0..40)
            .map(|i| TwitterQuery {
                q: format!("term{i}"),
                weight: 5,
            })
            .collect();
        let a = plan_twitter_searches_at(&pool, 1);
        let b = plan_twitter_searches_at(&pool, 2);
        assert_eq!(a.searches.len(), MAX_SEARCHES_PER_RUN);
        assert_eq!(b.searches.len(), MAX_SEARCHES_PER_RUN);
        assert_ne!(a.searches, b.searches);
        let ia = pool.iter().position(|q| q.q == a.searches[0].q).unwrap();
        let last = a.searches.len() - 1;
        let ib = pool.iter().position(|q| q.q == a.searches[last].q).unwrap();
        assert!(ia.abs_diff(ib) >= 4);
    }

    #[test]
    fn rustish_gets_gaming_excludes() {
        let q = apply_excludes_if_rustish("#rust", &default_excludes());
        assert!(q.contains("-RustGame"));
        assert!(q.contains("-RustClips"));
        assert!(q.contains("-GamingYT"));
        assert!(!q.contains("-#RustGame"));
        let again = apply_excludes_if_rustish(&q, &default_excludes());
        assert_eq!(again.matches("-RustGame").count(), 1);
    }

    #[test]
    fn query_for_log_strips_minus_and_lang() {
        let raw = apply_excludes_if_rustish("#rust lang:en", &default_excludes());
        let surface = query_for_log(&raw);
        assert_eq!(surface, "#rust");
        assert!(!surface.contains("RustGame"));
        assert!(!surface.contains("lang:en"));
    }

    #[test]
    fn non_rust_skips_excludes() {
        let q = apply_excludes_if_rustish("casper x402", &default_excludes());
        assert_eq!(q, "casper x402");
    }
}

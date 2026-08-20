// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Deterministic draft header: strong Draft ID + up to 3 link options for Slack rework.

use crate::sqlite::open_configured;
use chrono::Local;
use rusqlite::params;
use std::fmt::Write;
use std::path::Path;
use thiserror::Error;

const CODE_SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS draft_code_seq (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    next_ord INTEGER NOT NULL
);
INSERT OR IGNORE INTO draft_code_seq (id, next_ord) VALUES (1, 0);
";

#[derive(Debug, Error)]
pub enum DraftFooterError {
    #[error("draft code store: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("draft code: {0}")]
    Other(String),
}

/// Allocate next draft id: `DRAFT-YYYYMMDD-NNNNNN` (monotonic; never wraps).
/// Persisted sequence lives in the state DB (`draft_code_seq`).
///
/// # Errors
///
/// Returns [`DraftFooterError::Db`] on `SQLite` failure, or [`DraftFooterError::Other`] for path/IO failures.
pub fn next_draft_id(db_path: &Path) -> Result<String, DraftFooterError> {
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DraftFooterError::Other(format!("create parent: {e}")))?;
        }
    }
    let conn = open_configured(db_path)?;
    conn.execute_batch(CODE_SCHEMA)?;
    let ord: i64 = conn.query_row(
        "SELECT next_ord FROM draft_code_seq WHERE id = 1",
        [],
        |r| r.get(0),
    )?;
    let next = ord.saturating_add(1);
    conn.execute(
        "UPDATE draft_code_seq SET next_ord = ?1 WHERE id = 1",
        params![next],
    )?;
    let day = Local::now().format("%Y%m%d");
    // Display seq is 1-based for humans (first draft of a fresh DB = ...-000001).
    let seq = next;
    Ok(format!("DRAFT-{day}-{seq:06}"))
}

/// Prefer unique **verified pack** URLs. A body cite wins the first slot only when it is
/// already in the pack (writer chose a real candidate). Never promote invented body-only URLs.
#[must_use]
pub fn pick_link_options(pack_urls: &[String], body: &str) -> Vec<String> {
    use crate::sources::url_hygiene::{
        extract_https_urls, filter_publisher_urls, is_junk_or_search_url, same_publisher_url,
        scrub_https_url, url_in_allowlist,
    };

    let pack = filter_publisher_urls(pack_urls);
    let body_urls: Vec<String> = extract_https_urls(body)
        .into_iter()
        .filter(|u| !is_junk_or_search_url(u) && url_in_allowlist(u, &pack))
        .collect();
    let mut out: Vec<String> = Vec::new();
    for u in body_urls.iter().chain(pack.iter()) {
        let scrubbed = scrub_https_url(u);
        if !scrubbed.starts_with("https://") {
            continue;
        }
        if out.iter().any(|x| same_publisher_url(x, &scrubbed)) {
            continue;
        }
        out.push(scrubbed);
        if out.len() == 3 {
            break;
        }
    }
    out
}

/// If the body has no https publisher URL but we have options, insert the primary https line.
#[must_use]
pub fn ensure_primary_link_line(body: &str, primary: Option<&str>) -> String {
    let Some(primary) = primary else {
        return body.to_string();
    };
    if body
        .lines()
        .any(crate::sources::draft_url::is_in_post_https_line)
    {
        return body.to_string();
    }
    for marker in crate::sources::draft_url::PRIMARY_LINK_INSERT_MARKERS {
        if let Some(idx) = body.find(marker) {
            let (head, tail) = body.split_at(idx);
            return format!("{}\n{}\n\n{}", head.trim_end(), primary, tail);
        }
    }
    format!("{}\n\n{}", body.trim_end(), primary)
}

/// Put Draft ID first (operator reference), then body, then `Link: N` / `0`.
#[must_use]
pub fn compose_draft_message(body: &str, draft_id: &str, links: &[String]) -> String {
    let body = crate::sources::draft_url::strip_sources_section(body);
    let in_post = crate::sources::draft_url::extract_in_post_url(&body);
    let mut out = format!("Draft ID: {draft_id}\n\n");
    out.push_str(body.trim());
    out.push_str("\n\n");
    match (
        &in_post,
        in_post.as_deref().and_then(|url| {
            links
                .iter()
                .position(|u| crate::sources::url_hygiene::same_publisher_url(u, url))
                .map(|i| i + 1)
        }),
    ) {
        (Some(_), Some(n)) => {
            let _ = writeln!(out, "Link: {n}");
        }
        (Some(url), None) => {
            let _ = writeln!(out, "Link: {url}");
        }
        (None, _) => {
            let _ = writeln!(out, "Link: 0");
        }
    }
    let _ = writeln!(out, "0 = no link. /change_url {draft_id} <0|1|2|3|url>");
    for (i, u) in links.iter().enumerate() {
        let _ = writeln!(out, "{}. {u}", i + 1);
    }
    out.trim_end().to_string()
}

/// Simplified draft message for self-introduction commands: Draft ID + body only, no link picker.
///
/// The URL is already baked into the body by the self-intro writer. Storing `link_options`
/// separately still allows `/accept` and `/rework` to work correctly.
#[must_use]
pub fn compose_self_intro_draft_message(body: &str, draft_id: &str) -> String {
    let body = crate::sources::draft_url::strip_sources_section(body);
    format!("Draft ID: {draft_id}\n\n{}", body.trim())
}

/// Prose only from a stored Slack draft (drop Draft ID, Link footer, disclosure).
#[must_use]
pub fn draft_prose_for_rework(body: &str) -> String {
    let body = crate::llm::disclosure::strip_trailing_disclosures(body);
    let mut lines: Vec<&str> = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if is_draft_footer_break(t) {
            break;
        }
        if is_draft_operator_chrome(t) {
            continue;
        }
        lines.push(line);
    }
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n").trim().to_string()
}

fn is_draft_footer_break(t: &str) -> bool {
    t.starts_with("Link:")
        || t.starts_with("0 = no link")
        || t.starts_with("Written by AI")
        || t.starts_with("Next:")
}

fn is_draft_operator_chrome(t: &str) -> bool {
    t.starts_with("Draft ID:")
        || t.starts_with("Draft code:")
        || t.starts_with("Saved as")
        || t.starts_with("Reworked draft")
        || t.starts_with("Sources")
        || (t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains("http"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn draft_ids_monotonic_and_shaped() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("s.db");
        let a = next_draft_id(&path).expect("a");
        let b = next_draft_id(&path).expect("b");
        assert!(a.starts_with("DRAFT-"));
        assert!(a.contains(&Local::now().format("%Y%m%d").to_string()));
        assert!(a.ends_with("-000001"));
        assert!(b.ends_with("-000002"));
        assert_ne!(a, b);
    }

    #[test]
    fn pick_three_links_prefers_verified_body_cite() {
        let pack = vec![
            "https://labs.sogeti.com/a".into(),
            "https://wavespeed.ai/blog/b".into(),
            "https://blog.mean.ceo/c".into(),
        ];
        let body = "Hello\n\nhttps://wavespeed.ai/blog/b\n";
        let opts = pick_link_options(&pack, body);
        assert_eq!(opts[0], "https://wavespeed.ai/blog/b");
        assert_eq!(opts.len(), 3);
    }

    #[test]
    fn pick_dedupes_trailing_slash_and_backtick() {
        let pack = vec![
            "https://itsfoss.com/news/rust-code-repo-ai-policy".into(),
            "https://itsfoss.com/news/rust-code-repo-ai-policy/".into(),
            "https://linuxiac.com/rust-adopts-official-policy-for-ai-generated-contributions/"
                .into(),
        ];
        let body = "Post\n\nhttps://itsfoss.com/news/rust-code-repo-ai-policy`\n";
        let opts = pick_link_options(&pack, body);
        assert_eq!(opts.len(), 2, "{opts:?}");
        assert_eq!(opts[0], "https://itsfoss.com/news/rust-code-repo-ai-policy");
        assert!(opts[1].contains("linuxiac.com"));
        assert!(!opts.iter().any(|u| u.contains('`')));
    }

    #[test]
    fn pick_ignores_invented_body_url_not_in_pack() {
        let pack = vec![
            "https://labs.sogeti.com/token-tax".into(),
            "https://wavespeed.ai/blog/rtk".into(),
        ];
        let body = "Post\n\nhttps://www.example-news-site.com/rtk-ai-labs-ceo-update\n";
        let opts = pick_link_options(&pack, body);
        assert_eq!(opts[0], "https://labs.sogeti.com/token-tax");
        assert!(!opts.iter().any(|u| u.contains("example-news-site")));
    }

    #[test]
    fn ensure_inserts_primary_when_missing() {
        let body = "Commentary here.";
        let out = ensure_primary_link_line(body, Some("https://labs.sogeti.com/article"));
        assert!(out.contains("https://labs.sogeti.com/article"));
        assert!(out.contains("Commentary here."));
    }

    #[test]
    fn compose_strips_sources_keeps_one_link() {
        let body =
            "Post text\n\nhttps://labs.sogeti.com/one/\n\nSources:\n- https://x.com/a/status/1\n";
        let out = compose_draft_message(
            body,
            "DRAFT-20260728-000042",
            &["https://labs.sogeti.com/one/".into()],
        );
        assert!(!out.contains("Sources:"));
        assert!(out.contains("https://labs.sogeti.com/one/"));
        assert!(!out.contains("x.com/a/status"));
    }

    #[test]
    fn compose_puts_id_first() {
        let body = "Post text\n\nhttps://labs.sogeti.com/one/\n";
        let out = compose_draft_message(
            body,
            "DRAFT-20260728-000042",
            &[
                "https://labs.sogeti.com/one/".into(),
                "https://wavespeed.ai/two/".into(),
                "https://blog.mean.ceo/three/".into(),
            ],
        );
        assert!(out.starts_with("Draft ID: DRAFT-20260728-000042\n\n"));
        assert!(out.contains("Post text"));
        assert!(out.contains("1. https://labs.sogeti.com/one/"));
        assert!(out.contains("Link: 1"));
        assert!(out.contains("0 = no link"));
        assert!(out.find("Draft ID:").unwrap() < out.find("Post text").unwrap());
        assert!(out.find("Post text").unwrap() < out.find("Link: 1").unwrap());
    }

    #[test]
    fn draft_prose_for_rework_drops_slack_chrome() {
        let stored = compose_draft_message(
            "The contrast is stark. C/C++ CVEs are often weaponized.\n\nhttps://kobzol.github.io/rust/cve\n",
            "DRAFT-20260817-000056",
            &["https://kobzol.github.io/rust/cve".into()],
        );
        let prose = draft_prose_for_rework(&stored);
        assert!(prose.contains("The contrast is stark"));
        assert!(prose.contains("https://kobzol.github.io/rust/cve"));
        assert!(!prose.contains("Draft ID:"));
        assert!(!prose.contains("Link:"));
        assert!(!prose.contains("0 = no link"));
        assert!(!prose.contains("change_url"));
    }
}

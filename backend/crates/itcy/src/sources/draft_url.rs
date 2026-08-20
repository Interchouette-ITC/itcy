// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Deterministic in-post URL swap for `/change_url`.
//!
//! Drafts and tweets carry **at most one** bare `https://` line (before Link options /
//! disclosure). Markdown links are stripped. There is no Sources list.

use crate::sources::url_hygiene::is_linkedin_host;

/// True for a body line that is a non-LinkedIn `https://` publisher URL.
#[must_use]
pub fn is_in_post_https_line(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("https://") && !is_linkedin_host(t)
}

/// True when a line is only a markdown link or a bare https URL (drop on normalize).
#[must_use]
fn is_url_only_line(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    if is_in_post_https_line(t) {
        return true;
    }
    // `[label](https://…)` possibly alone on the line
    if t.starts_with('[') && t.contains("](https://") && t.ends_with(')') {
        return true;
    }
    false
}

/// Byte index of the operator footer (`Link options` / disclosure), if any.
#[must_use]
pub fn footer_start(body: &str) -> Option<usize> {
    let markers = [
        "\nLink options",
        "\nIn-post URL",
        "\nLink:",
        "\nX:",
        "\nCite =",
        "\nCite:",
        "\n0 = no link",
        "\n0 = no cite",
        "\nSources:",
        "\nSources used:",
        "\nWritten by AI",
    ];
    let mut best = None;
    for m in markers {
        if let Some(i) = body.find(m) {
            best = Some(best.map_or(i, |b: usize| b.min(i)));
        }
    }
    best
}

/// Drop a model-emitted `Sources:` / `Sources used:` block. Drafts and tweets have none.
#[must_use]
pub fn strip_sources_section(raw: &str) -> String {
    let markers = ["\nSources:", "\nSources used:"];
    let mut cut = raw.len();
    for m in markers {
        if let Some(i) = raw.find(m) {
            cut = cut.min(i);
        }
    }
    let head = raw[..cut].trim();
    for prefix in ["Sources:", "Sources used:"] {
        if let Some(rest) = head.strip_prefix(prefix) {
            return rest
                .lines()
                .skip_while(|l| {
                    let t = l.trim();
                    t.is_empty() || t.starts_with('-') || t.starts_with("http")
                })
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
        }
    }
    head.to_string()
}

/// Markers used when inserting a primary link before trailing chrome (if any).
pub const PRIMARY_LINK_INSERT_MARKERS: &[&str] = &["Sources:", "Sources used:", "Written by AI"];

/// Current in-post publisher URL (bare line preferred; else markdown href), before footer.
#[must_use]
pub fn extract_in_post_url(body: &str) -> Option<String> {
    let split_at = footer_start(body).unwrap_or(body.len());
    let head = &body[..split_at];
    for line in head.lines().rev() {
        if is_in_post_https_line(line) {
            return Some(line.trim().to_string());
        }
    }
    for line in head.lines().rev() {
        if let Some(u) = markdown_href(line) {
            if u.starts_with("https://") && !is_linkedin_host(&u) {
                return Some(u);
            }
        }
    }
    None
}

/// Move `url` to index 0 of `options` (insert if missing). Caps at 3.
pub fn promote_link_option(options: &mut Vec<String>, url: &str) {
    let url = crate::sources::url_hygiene::scrub_https_url(url);
    if url.is_empty() {
        return;
    }
    if let Some(i) = options
        .iter()
        .position(|u| crate::sources::url_hygiene::same_publisher_url(u, &url))
    {
        let u = options.remove(i);
        options.insert(0, u);
    } else {
        options.insert(0, url);
    }
    options.truncate(3);
}

/// Replace / normalize so the commentary head has **exactly one** bare https URL line.
/// Empty `new_url` clears all in-post https lines (no cite).
#[must_use]
pub fn set_single_in_post_url(body: &str, new_url: &str) -> String {
    let new_url = new_url.trim();
    let split_at = footer_start(body).unwrap_or(body.len());
    let (head, tail) = body.split_at(split_at);
    let mut lines: Vec<String> = Vec::new();
    for line in head.lines() {
        if is_url_only_line(line) {
            continue;
        }
        lines.push(strip_inline_markdown_links(line));
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if !new_url.is_empty() {
        lines.push(String::new());
        lines.push(crate::sources::url_hygiene::scrub_https_url(new_url));
    }
    let mut out = lines.join("\n");
    if !tail.is_empty() {
        out.push_str("\n\n");
        out.push_str(tail.trim_start());
    }
    out
}

/// Outcome of `/change_url` choice parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlChoice {
    /// Operator asked to remove the in-post / cite URL (`0`).
    Clear,
    /// Set this publisher or X status URL.
    Url(String),
}

/// Resolve change-url arg: `0` clears, `1`/`2`/`3` index into `link_options`, or raw https.
///
/// # Errors
///
/// Returns `Err(String)` with an operator-facing message when validation or lookup fails.
pub fn resolve_url_choice(arg: &str, link_options: &[String]) -> Result<UrlChoice, String> {
    let t = arg.trim();
    if t.is_empty() {
        return Err("need 0 (no link), link index 1-3, or an https URL".into());
    }
    if t == "0" {
        return Ok(UrlChoice::Clear);
    }
    if let Ok(n) = t.parse::<usize>() {
        if (1..=3).contains(&n) {
            return link_options
                .get(n - 1)
                .cloned()
                .map(UrlChoice::Url)
                .ok_or_else(|| format!("no link option {n} on this draft"));
        }
        return Err("link index must be 0 (clear), 1, 2, or 3".into());
    }
    if t.starts_with("https://") {
        return Ok(UrlChoice::Url(
            t.trim_end_matches(['.', ',', ')', ']']).to_string(),
        ));
    }
    Err("expected 0, 1, 2, 3, or an https:// URL".into())
}

fn markdown_href(line: &str) -> Option<String> {
    let start = line.find("](https://")?;
    let rest = &line[start + 2..];
    let end = rest.find(')')?;
    let u = rest[..end].trim();
    if u.starts_with("https://") {
        Some(u.to_string())
    } else {
        None
    }
}

fn strip_inline_markdown_links(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        if let Some(mid) = after.find("](https://") {
            if let Some(close) = after[mid..].find(')') {
                // Drop `[label](url)` entirely (URL is managed as the bare line).
                rest = &after[mid + close + 1..];
                continue;
            }
        }
        out.push('[');
        rest = &after[1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sources_keeps_commentary_and_link() {
        let body = "Hello builders.\n\nhttps://labs.sogeti.com/a\n\nSources:\n- https://x.com/a/status/1\n";
        let out = strip_sources_section(body);
        assert!(out.contains("Hello builders."));
        assert!(out.contains("https://labs.sogeti.com/a"));
        assert!(!out.contains("Sources"));
        assert!(!out.contains("x.com/a/status"));
    }

    #[test]
    fn swaps_primary_https() {
        let body = "Draft ID: X\n\nHello\n\nhttps://old.example/a\n\nLink options:\n1. https://old.example/a\n";
        let out = set_single_in_post_url(body, "https://new.example/b");
        assert!(out.contains("https://new.example/b"));
        assert!(!out
            .split("Link options")
            .next()
            .unwrap()
            .contains("https://old.example/a"));
        assert!(out.contains("Link options"));
    }

    #[test]
    fn strips_markdown_and_keeps_one_bare_url() {
        let body = "Hello\n\n[https://old.example/a](https://old.example/a)\nhttps://old.example/a\n\nSources:\n- x\n";
        let out = set_single_in_post_url(body, "https://new.example/b");
        let head = out.split("Sources:").next().unwrap();
        assert_eq!(
            head.lines()
                .filter(|l| l.trim().starts_with("https://"))
                .count(),
            1
        );
        assert!(head.contains("https://new.example/b"));
        assert!(!head.contains("old.example"));
        assert!(!head.contains("]("));
    }

    #[test]
    fn promote_moves_to_front() {
        let mut opts = vec![
            "https://a.example/1".into(),
            "https://b.example/2".into(),
            "https://c.example/3".into(),
        ];
        promote_link_option(&mut opts, "https://c.example/3");
        assert_eq!(opts[0], "https://c.example/3");
        assert_eq!(opts.len(), 3);
    }

    #[test]
    fn resolve_index_and_url() {
        let opts = vec!["https://a.example/1".into(), "https://b.example/2".into()];
        assert_eq!(
            resolve_url_choice("2", &opts).unwrap(),
            UrlChoice::Url("https://b.example/2".into())
        );
        assert_eq!(
            resolve_url_choice("https://c.example/x", &opts).unwrap(),
            UrlChoice::Url("https://c.example/x".into())
        );
        assert_eq!(resolve_url_choice("0", &opts).unwrap(), UrlChoice::Clear);
        assert!(resolve_url_choice("9", &opts).is_err());
    }

    #[test]
    fn clear_strips_in_post_url() {
        let body = "Hello\n\nhttps://old.example/a\n\nSources:\n- x\n";
        let out = set_single_in_post_url(body, "");
        let head = out.split("Sources:").next().unwrap();
        assert!(!head.contains("https://"));
        assert!(head.contains("Hello"));
    }
}

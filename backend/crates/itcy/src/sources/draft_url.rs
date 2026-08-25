// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Deterministic in-post URL swap for `/change_url`.
//!
//! Drafts keep **at most one** bare `https://` line. Tweets keep one by default; when the
//! operator listed multiple https URLs, each required URL is a bare line (primary last).
//! Markdown links are stripped. There is no Sources list.

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
    options.retain(|u| {
        !crate::sources::url_hygiene::same_publisher_url(u, &url)
            && !crate::sources::url_hygiene::same_publisher_domain(u, &url)
    });
    options.insert(0, url);
    options.truncate(3);
}

/// Replace / normalize so the commentary head has **exactly one** bare https URL line.
/// Empty `new_url` clears all in-post https lines (no cite).
#[must_use]
pub fn set_single_in_post_url(body: &str, new_url: &str) -> String {
    let urls = if new_url.trim().is_empty() {
        Vec::new()
    } else {
        vec![crate::sources::url_hygiene::scrub_https_url(new_url)]
    };
    set_in_post_https_lines(body, &urls)
}

/// Write zero or more bare https lines after commentary (primary / Link cite last).
///
/// Drops prior URL-only lines and bare `https://` tokens glued onto prose (model often
/// ends a paragraph with the cite URL). Then appends each URL on its own line.
#[must_use]
pub fn set_in_post_https_lines(body: &str, urls: &[String]) -> String {
    let split_at = footer_start(body).unwrap_or(body.len());
    let (head, tail) = body.split_at(split_at);
    let mut lines: Vec<String> = Vec::new();
    for line in head.lines() {
        if is_url_only_line(line) {
            continue;
        }
        // Keep blank lines (paragraph aeration). Only drop lines that had content but
        // became empty after stripping an inline cite URL.
        if line.trim().is_empty() {
            lines.push(String::new());
            continue;
        }
        let cleaned = strip_bare_https_tokens(&strip_inline_markdown_links(line));
        if cleaned.trim().is_empty() {
            continue;
        }
        lines.push(cleaned);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut seen: Vec<String> = Vec::new();
    for u in urls {
        let scrubbed = crate::sources::url_hygiene::scrub_https_url(u);
        if scrubbed.is_empty() {
            continue;
        }
        if seen
            .iter()
            .any(|x| crate::sources::url_hygiene::same_publisher_url(x, &scrubbed))
        {
            continue;
        }
        seen.push(scrubbed);
    }
    if !seen.is_empty() {
        lines.push(String::new());
        for (i, u) in seen.iter().enumerate() {
            if i > 0 {
                lines.push(String::new());
            }
            lines.push(u.clone());
        }
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
        // `[label](href)` - https href: drop whole token (bare line owns the URL);
        // empty / non-https href: keep label as plain text.
        if let Some(mid) = after.find("](") {
            let label = &after[1..mid];
            let href_start = mid + 2;
            if let Some(close_rel) = after[href_start..].find(')') {
                let href = after[href_start..href_start + close_rel].trim();
                let after_token = &after[href_start + close_rel + 1..];
                if href.starts_with("https://") {
                    rest = after_token;
                    continue;
                }
                out.push_str(label);
                rest = after_token;
                continue;
            }
        }
        out.push('[');
        rest = &after[1..];
    }
    out.push_str(rest);
    out
}

/// Drop bare `https://…` tokens from a commentary line (keeps surrounding prose).
fn strip_bare_https_tokens(line: &str) -> String {
    let urls = crate::sources::url_hygiene::extract_https_urls(line);
    if urls.is_empty() {
        return line.to_string();
    }
    let mut out = line.to_string();
    for u in urls {
        out = out.replace(&u, "");
    }
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    out.trim().to_string()
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
    fn strips_trailing_inline_https_keeps_one_bare() {
        let cite = "https://x.com/mmalisper/status/2091925981363941499";
        let body = format!(
            "And if you're using Postgres, it's worth a closer look. {cite}\n\n{cite}\n\nLink: 1\n"
        );
        let out = set_single_in_post_url(&body, cite);
        let head = out.split("\nLink:").next().unwrap();
        assert_eq!(
            head.matches(cite).count(),
            1,
            "prose+bare duplicate must collapse: {head}"
        );
        assert!(
            head.lines().any(|l| l.trim() == cite),
            "cite must be its own line: {head}"
        );
        assert!(head.contains("closer look"), "prose must survive: {head}");
        assert!(
            !head.contains("look. http"),
            "must not leave glue before URL: {head}"
        );
    }

    #[test]
    fn preserves_paragraph_blank_lines_when_normalizing_cite() {
        let cite =
            "https://malisper.me/how-we-made-postgres-hundreds-of-times-faster-the-query-engine";
        let body = format!(
            "Para one with a win.\n\nPara two about rhythm.\n\nPara three closer look. {cite}\n\nLink: 1\n"
        );
        let out = set_single_in_post_url(&body, cite);
        let head = out.split("\nLink:").next().unwrap();
        assert!(
            head.contains(
                "Para one with a win.\n\nPara two about rhythm.\n\nPara three closer look."
            ),
            "aeration must survive cite normalize: {head:?}"
        );
        assert_eq!(head.matches(cite).count(), 1);
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
    fn empty_markdown_keeps_label_and_one_bare_url() {
        let cite = "https://github.com/Interchouette-ITC/rangular";
        let body =
            format!("The project, [rangular](), is a tiny experiment.\n\n{cite}\n\nLink: 1\n");
        let out = set_single_in_post_url(&body, cite);
        let head = out.split("\nLink:").next().unwrap();
        assert!(head.contains("rangular"));
        assert!(!head.contains("[rangular]"));
        assert!(!head.contains("]()"));
        assert!(!head.contains("]("));
        assert_eq!(
            head.lines()
                .filter(|l| l.trim().starts_with("https://"))
                .count(),
            1
        );
        assert!(head.contains(cite));
    }

    #[test]
    fn non_https_markdown_keeps_label() {
        let out = strip_inline_markdown_links("see [docs](relative/path) now");
        assert_eq!(out, "see docs now");
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
    fn promote_replaces_same_domain_and_keeps_three_unique() {
        let digest = "https://decrypt.co/376271/chatgpt-web-ai-written-pew";
        let mut opts = vec![
            "https://decrypt.co/old-path".into(),
            "https://www.pewresearch.org/data-labs/2026/08/20/how-much-of-the-internet-is-written-with-ai/"
                .into(),
            "https://techcrunch.com/2026/08/20/a-third-of-webpages-published-since-chatgpts-launch-show-signs-of-ai-authorship-study-finds/"
                .into(),
        ];
        promote_link_option(&mut opts, digest);
        assert_eq!(opts.len(), 3, "{opts:?}");
        assert_eq!(opts[0], digest);
        assert_eq!(opts.iter().filter(|u| u.contains("decrypt.co")).count(), 1);
        let hosts: std::collections::HashSet<_> = opts
            .iter()
            .filter_map(|u| crate::sources::url_hygiene::publisher_host(u))
            .collect();
        assert_eq!(hosts.len(), 3, "{opts:?}");
    }

    #[test]
    fn promote_inserts_forced_cite_when_missing_from_options() {
        let digest = "https://decrypt.co/376271/chatgpt-web-ai-written-pew";
        let mut opts = vec![
            "https://www.pewresearch.org/data-labs/2026/08/20/how-much-of-the-internet-is-written-with-ai/"
                .into(),
            "https://techcrunch.com/2026/08/20/a-third-of-webpages-published-since-chatgpts-launch-show-signs-of-ai-authorship-study-finds/"
                .into(),
        ];
        promote_link_option(&mut opts, digest);
        assert_eq!(opts[0], digest);
        assert_eq!(opts.len(), 3, "{opts:?}");
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

    #[test]
    fn multi_https_lines_primary_last() {
        let body = "Beats\n\nhttps://x.com/a/status/1\n";
        let out = set_in_post_https_lines(
            body,
            &[
                "https://github.com/Interchouette-ITC/evaluator".into(),
                "https://x.com/a/status/1".into(),
            ],
        );
        let head = out.split("Link:").next().unwrap_or(&out);
        let https: Vec<_> = head
            .lines()
            .filter(|l| l.trim().starts_with("https://"))
            .map(str::trim)
            .collect();
        assert_eq!(https.len(), 2);
        assert_eq!(https[0], "https://github.com/Interchouette-ITC/evaluator");
        assert_eq!(https[1], "https://x.com/a/status/1");
    }
}

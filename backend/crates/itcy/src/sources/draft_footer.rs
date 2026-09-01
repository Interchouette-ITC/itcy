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

/// Prefer unique **verified pack** URLs for Link options.
///
/// A body cite wins the first slot only when it is already in the pack (writer chose a
/// real candidate). Never promote invented body-only URLs. Keeps up to
/// [`crate::sources::publisher_url::LINK_OPTIONS_CAP`] distinct domains.
#[must_use]
pub fn pick_link_options(pack_urls: &[String], body: &str) -> Vec<String> {
    use crate::sources::publisher_url::LINK_OPTIONS_CAP;
    use crate::sources::url_hygiene::{
        extract_https_urls, filter_publisher_urls, is_junk_or_search_url, same_publisher_domain,
        scrub_https_url, should_replace_same_host_url, url_in_allowlist,
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
        if crate::sources::url_hygiene::publisher_host(&scrubbed).is_none() {
            continue;
        }
        if let Some(idx) = out.iter().position(|x| same_publisher_domain(x, &scrubbed)) {
            if should_replace_same_host_url(&out[idx], &scrubbed) {
                out[idx] = scrubbed;
            }
            continue;
        }
        out.push(scrubbed);
        if out.len() == LINK_OPTIONS_CAP {
            break;
        }
    }
    out
}

/// Ensure a bare publisher https line with a blank line before it.
///
/// When the body already has an in-post cite, keep that URL and normalize spacing.
/// When missing, insert `primary` via [`crate::sources::draft_url::set_single_in_post_url`].
#[must_use]
pub fn ensure_primary_link_line(body: &str, primary: Option<&str>) -> String {
    let Some(primary) = primary else {
        return body.to_string();
    };
    if body
        .lines()
        .any(crate::sources::draft_url::is_in_post_https_line)
    {
        return crate::sources::draft_url::extract_in_post_url(body).map_or_else(
            || body.to_string(),
            |url| crate::sources::draft_url::set_single_in_post_url(body, &url),
        );
    }
    crate::sources::draft_url::set_single_in_post_url(body, primary)
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
    let _ = writeln!(out, "0 = no link. /change_url {draft_id} <0|1|2|3|4|5|url>");
    for (i, u) in links.iter().enumerate() {
        let _ = writeln!(out, "{}. {u}", i + 1);
    }
    out.trim_end().to_string()
}

/// Slack display only: fence `LinkedIn` draft prose so emoji copy as Unicode text.
///
/// Slack turns emoji into rich widgets / images on the clipboard; `LinkedIn` paste then
/// drops them (blank). A code block keeps glyphs as plain text. Do **not** store the
/// fenced form in `runtime.db` or publications `body.md`.
#[must_use]
pub fn slack_paste_safe_linkedin_message(composed: &str) -> String {
    let Some((header, rest)) = composed.split_once("\n\n") else {
        return slack_highlight_active_link(composed);
    };
    if !header.starts_with("Draft ID:") {
        return slack_highlight_active_link(composed);
    }
    let (prose, footer) = rest
        .find("\nLink: ")
        .or_else(|| rest.find("\n0 = no link"))
        .map_or((rest, ""), |i| (&rest[..i], &rest[i..]));
    let prose = prose.trim();
    if prose.is_empty() || prose.starts_with("```") {
        return slack_highlight_active_link(composed);
    }
    // On-the-fly: old stored drafts may still have spaced-hyphen pauses.
    let prose = crate::llm::sanitize_itcy_text(prose);
    let fenced = fence_slack_plaintext(&prose);
    let shown = if footer.is_empty() {
        format!("{header}\n\n{fenced}")
    } else {
        format!("{header}\n\n{fenced}{footer}")
    };
    slack_highlight_active_link(&shown)
}

/// Slack display only: operator chrome with shortcode emojis (`:dart:`, `:one:`, …).
///
/// Stored bodies stay plain. Ship / `body.md` / `LinkedIn` paste blocks are unchanged.
#[must_use]
pub fn slack_highlight_active_link(composed: &str) -> String {
    if composed.lines().any(|l| {
        let t = l.trim();
        t.starts_with("Link: :") || t.starts_with(":memo: Draft ID:")
    }) {
        return composed.to_string();
    }
    let active = active_link_index(composed);
    let mut out = String::with_capacity(composed.len() + 64);
    for line in composed.lines() {
        if let Some(highlighted) = decorate_slack_chrome_line(line, active) {
            out.push_str(&highlighted);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if composed.ends_with('\n') {
        out
    } else {
        out.trim_end_matches('\n').to_string()
    }
}

fn active_link_index(composed: &str) -> Option<u8> {
    for line in composed.lines() {
        let t = strip_leading_slack_emoji(line.trim());
        let Some(rest) = t.strip_prefix("Link:") else {
            continue;
        };
        return parse_link_selection(rest.trim());
    }
    None
}

fn parse_link_selection(rest: &str) -> Option<u8> {
    let rest = strip_leading_slack_emoji(rest.trim());
    if rest.starts_with("http://") || rest.starts_with("https://") {
        return None;
    }
    let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
    digits.parse::<u8>().ok().filter(|n| *n <= 4)
}

fn strip_leading_slack_emoji(s: &str) -> &str {
    let mut rest = s.trim_start();
    while rest.starts_with(':') {
        let Some(end) = rest[1..].find(':') else {
            break;
        };
        let name = &rest[1..=end];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            break;
        }
        rest = rest[end + 2..].trim_start();
    }
    rest
}

const fn slack_num_emoji(n: u8) -> &'static str {
    match n {
        0 => ":zero:",
        1 => ":one:",
        2 => ":two:",
        3 => ":three:",
        4 => ":four:",
        _ => "",
    }
}

fn decorate_slack_chrome_line(line: &str, active: Option<u8>) -> Option<String> {
    let t = line.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(id) = t.strip_prefix("Draft ID:") {
        return Some(format!(":memo: Draft ID:{id}"));
    }
    if let Some(id) = t.strip_prefix("Tweet ID:") {
        return Some(format!(":bird: Tweet ID:{id}"));
    }
    if let Some(rest) = t.strip_prefix("Link:") {
        let rest = rest.trim();
        if let Some(n) = parse_link_selection(rest) {
            let em = slack_num_emoji(n);
            return Some(format!("Link: {em}"));
        }
        return Some(format!("Link: {rest}"));
    }
    if t.starts_with("0 = no link") {
        return Some(format!(":zero: {t}"));
    }
    if let Some((n, url)) = parse_numbered_option(t) {
        let em = slack_num_emoji(n);
        if active == Some(n) && n >= 1 {
            // Selected cite: number icon + target + URL (no repeated digit).
            return Some(format!("{em} :dart: {url}"));
        }
        return Some(format!("{em} {url}"));
    }
    None
}

fn parse_numbered_option(t: &str) -> Option<(u8, &str)> {
    let dot = t.find(". ")?;
    let n: u8 = t[..dot].parse().ok()?;
    if !(1..=4).contains(&n) {
        return None;
    }
    Some((n, &t[dot + 2..]))
}

/// Manual company-page paste: stripped body + disclosure with `<in:… out:…>`, Slack-fenced.
///
/// No `Draft ID`, no `Link:` chrome. Copy only the fenced block into `LinkedIn`.
#[must_use]
pub fn linkedin_manual_paste_message(
    body: &str,
    model: &str,
    tokens_in: u32,
    tokens_out: u32,
) -> String {
    let cleaned = crate::publish::linkedin_text_for_api(body);
    let without = crate::llm::disclosure::strip_trailing_disclosures(&cleaned).trim_end();
    let text = if model.trim().is_empty() && tokens_in == 0 && tokens_out == 0 {
        without.to_string()
    } else {
        format!(
            "{without}\n\n{}",
            crate::llm::disclosure::format_disclosure_paste(model, tokens_in, tokens_out)
        )
    };
    let fenced = fence_slack_plaintext(&text);
    format!(
        ":clipboard: LinkedIn paste (copy the block only; playground = paste on company Page):\n{fenced}"
    )
}

/// Fence plain text for Slack copy (Unicode emoji survive clipboard paste).
#[must_use]
pub fn fence_slack_plaintext(inner: &str) -> String {
    let ticks = if inner.contains("```") { "````" } else { "```" };
    format!("{ticks}\n{}\n{ticks}", inner.trim_end())
}

/// Slack display for CREPLY/XREPLY bodies: expand shortcodes, then fence for copy.
#[must_use]
pub fn slack_paste_safe_reply_body(reply: &str) -> String {
    let prose = crate::llm::sanitize_itcy_text(reply.trim());
    if prose.is_empty() {
        return String::new();
    }
    if prose.starts_with("```") {
        return prose;
    }
    fence_slack_plaintext(&prose)
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
        if t.starts_with("```") {
            continue;
        }
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

/// Split dense `LinkedIn` wall text into 2-4 paragraphs (`\n\n` between blocks).
///
/// Preserves existing paragraph breaks. Unlike X tweet aeration, each block stays
/// multi-sentence (dense `LinkedIn` shape per Form craft).
#[must_use]
pub fn aerate_linkedin_draft(text: &str) -> String {
    let text = crate::sources::tweet_footer::join_soft_wrap_lines(text.trim());
    if text.is_empty() || text.contains("\n\n") {
        return text;
    }
    let sentences = merge_trailing_decor_fragments(split_prose_sentences(&text));
    if sentences.len() <= 1 {
        return text;
    }
    let ranges = linkedin_paragraph_ranges(&sentences);
    let mut out = String::new();
    for (start, end) in ranges {
        let block = sentences[start..end].join(" ");
        if block.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(block.trim());
    }
    if out.is_empty() {
        text
    } else {
        out
    }
}

fn split_prose_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut sentences = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if matches!(ch, '.' | '!' | '?') {
            let mut end = i + 1;
            if end < chars.len() && matches!(chars[end], '"' | '\'') {
                end += 1;
            }
            if end < chars.len() && chars[end] == ' ' {
                let next = chars.get(end + 1).copied();
                if next.is_some_and(sentence_starts_new_thought) {
                    let remainder: String = chars[end + 1..].iter().collect();
                    if is_trailing_decor_only(&remainder) {
                        i += 1;
                        continue;
                    }
                    let s: String = chars[start..end].iter().collect();
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        sentences.push(trimmed.to_string());
                    }
                    start = end + 1;
                    i = start;
                    continue;
                }
            }
        }
        i += 1;
    }
    let tail: String = chars[start..].iter().collect();
    if !tail.trim().is_empty() {
        sentences.push(tail.trim().to_string());
    }
    sentences
}

fn sentence_starts_new_thought(c: char) -> bool {
    c.is_uppercase() || (!c.is_ascii() && !c.is_alphanumeric())
}

fn is_trailing_decor_only(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty() && !t.chars().any(char::is_alphabetic)
}

fn merge_trailing_decor_fragments(mut sentences: Vec<String>) -> Vec<String> {
    while sentences.len() >= 2 {
        let last = sentences.last().expect("len >= 2");
        if is_trailing_decor_only(last) {
            let frag = sentences.pop().expect("len >= 2");
            let prev = sentences.last_mut().expect("len >= 1");
            if !prev.ends_with(' ') {
                prev.push(' ');
            }
            prev.push_str(frag.trim());
        } else {
            break;
        }
    }
    sentences
}

fn linkedin_paragraph_ranges(sentences: &[String]) -> Vec<(usize, usize)> {
    let n = sentences.len();
    if n <= 1 {
        return vec![(0, n)];
    }
    if n == 2 {
        return vec![(0, 1), (1, 2)];
    }
    let close_start = if sentences
        .last()
        .is_some_and(|s| linkedin_closing_sentence(s))
        && n >= 4
    {
        n - 1
    } else {
        n
    };
    let open_end = if n >= 4 { 2 } else { 1 };
    let mut ranges = Vec::new();
    if open_end > 0 {
        ranges.push((0, open_end.min(n)));
    }
    if close_start > open_end {
        ranges.push((open_end, close_start));
    }
    if close_start < n {
        ranges.push((close_start, n));
    }
    if ranges.is_empty() {
        return vec![(0, n)];
    }
    if ranges.len() == 1 && n >= 3 {
        let mid = n / 2;
        return vec![(0, mid), (mid, n)];
    }
    ranges
}

fn linkedin_closing_sentence(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("The future")
        || t.starts_with("The lesson")
        || t.starts_with("The takeaway")
        || t.starts_with("Overall,")
        || t.starts_with("In short,")
}

/// Drop Playwright a11y `Page Title:` lines so the writer cannot paste them as the lede.
#[must_use]
pub fn strip_browse_page_title_chrome(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let low = line.trim().to_ascii_lowercase();
            !(low.starts_with("- page title:") || low.starts_with("page title:"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drop a pasted page-title first line (`Title | @handle date` / `Title | Publisher`).
#[must_use]
pub fn strip_leading_page_title_lede(body: &str) -> String {
    let mut lines: Vec<&str> = body.lines().collect();
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    if lines
        .first()
        .is_some_and(|l| looks_like_page_title_lede(l.trim()))
    {
        lines.remove(0);
        while lines.first().is_some_and(|l| l.trim().is_empty()) {
            lines.remove(0);
        }
    }
    lines.join("\n").trim().to_string()
}

fn looks_like_page_title_lede(t: &str) -> bool {
    if t.len() < 24 {
        return false;
    }
    // Real LinkedIn ledes open with a context emoji (📜 …), not a CMS title.
    if let Some(c) = t.chars().next() {
        if !c.is_ascii() && !c.is_alphanumeric() && c != '@' && c != '#' {
            return false;
        }
    }
    if t.contains(" | @") {
        return true;
    }
    if let Some((_, right)) = t.rsplit_once(" | ") {
        let r = right.trim();
        if r.len() <= 48 && !r.contains('.') && r.split_whitespace().count() <= 6 {
            return true;
        }
    }
    false
}

/// Remove substrings the operator quoted in `/rework` instructions (hard, not model hope).
#[must_use]
pub fn strip_rework_quoted_removals(body: &str, instructions: &str) -> String {
    let mut out = body.to_string();
    for phrase in quoted_phrases_in_instructions(instructions) {
        if phrase.chars().count() < 16 {
            continue;
        }
        out = out.replace(&phrase, "");
    }
    collapse_blank_lines(&out).trim().to_string()
}

fn quoted_phrases_in_instructions(instructions: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (open, close) in [
        ('"', '"'),
        ('\u{201c}', '\u{201d}'),
        ('\u{2018}', '\u{2019}'),
    ] {
        let mut rest = instructions;
        while let Some(start) = rest.find(open) {
            let after = &rest[start + open.len_utf8()..];
            let Some(end) = after.find(close) else {
                break;
            };
            let phrase = after[..end].trim();
            if !phrase.is_empty() {
                out.push(phrase.to_string());
            }
            rest = &after[end + close.len_utf8()..];
        }
    }
    out
}

fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::new();
    let mut blank = 0_u8;
    for line in s.lines() {
        if line.trim().is_empty() {
            blank = blank.saturating_add(1);
            if blank <= 2 {
                out.push('\n');
            }
        } else {
            blank = 0;
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn is_draft_footer_break(t: &str) -> bool {
    let t = strip_leading_slack_emoji(t);
    t.starts_with("Link:")
        || t.starts_with("0 = no link")
        || t.starts_with("Written by AI")
        || t.starts_with("Next:")
}

fn is_draft_operator_chrome(t: &str) -> bool {
    let t = strip_leading_slack_emoji(t);
    t.starts_with("Draft ID:")
        || t.starts_with("Tweet ID:")
        || t.starts_with("Draft code:")
        || t.starts_with("Saved as")
        || t.starts_with("Reworked")
        || t.starts_with("Sources")
        || (t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains("http"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::draft_url::promote_link_option;
    use crate::sources::url_hygiene::publisher_host;
    use tempfile::TempDir;

    /// Same finalize path as `build_grounded_draft_with_cite` / `/propose_draft`.
    fn propose_link_options(pack: &[String], body: &str, forced_cite: &str) -> Vec<String> {
        let mut opts = pick_link_options(pack, body);
        promote_link_option(&mut opts, forced_cite);
        opts
    }

    fn assert_three_unique_domains(opts: &[String]) {
        assert!(
            opts.len() >= 3,
            "need at least 3 Link options, got {opts:?}"
        );
        let hosts: std::collections::HashSet<_> =
            opts.iter().filter_map(|u| publisher_host(u)).collect();
        assert!(
            hosts.len() >= 3,
            "need at least 3 distinct site hosts, got {opts:?}"
        );
    }

    #[test]
    fn pick_keeps_up_to_five_unique_domains() {
        let pack = vec![
            "https://decrypt.co/a".into(),
            "https://pewresearch.org/b".into(),
            "https://techcrunch.com/c".into(),
            "https://labs.sogeti.com/d".into(),
            "https://hacks.mozilla.org/e".into(),
            "https://arstechnica.com/f".into(),
        ];
        let opts = pick_link_options(&pack, "");
        assert_eq!(opts.len(), 5, "{opts:?}");
    }

    #[test]
    fn pick_link_options_floor_three_when_pack_has_three_domains() {
        let pack = vec![
            "https://x.com/a/status/1".into(),
            "https://labs.sogeti.com/a".into(),
            "https://decrypt.co/1/b".into(),
        ];
        let opts = pick_link_options(&pack, "");
        assert!(
            opts.len() >= crate::sources::publisher_url::LINK_OPTIONS_MIN,
            "LinkedIn Link options floor is 3: {opts:?}"
        );
    }

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
    fn pick_same_host_prefers_newer_dated_path() {
        let pack = vec![
            "https://www.scylladb.com/2025/03/26/scylladb-rust-driver-1-0/".into(),
            "https://www.scylladb.com/2026/08/27/new-rust-driver-for-scylladbs-dynamodb-api/".into(),
            "https://futurumgroup.com/insights/scylladbs-rust-driver-delivers-58-throughput-gain-for-dynamodb-users/".into(),
        ];
        let opts = pick_link_options(&pack, "");
        assert_eq!(opts.len(), 2, "{opts:?}");
        assert!(opts[0].contains("2026/08/27"), "{opts:?}");
        assert!(opts[1].contains("futurumgroup.com"), "{opts:?}");
    }

    #[test]
    fn pick_dedupes_same_publisher_domain() {
        let pack = vec![
            "https://decrypt.co/376271/chatgpt-web-ai-written-pew".into(),
            "https://www.pewresearch.org/data-labs/2026/08/20/how-much-of-the-internet-is-written-with-ai/"
                .into(),
            "https://www.pewresearch.org/data-labs/2026/08/20/methodology-ai-content/".into(),
            "https://techcrunch.com/2026/08/20/a-third-of-webpages-published-since-chatgpts-launch-show-signs-of-ai-authorship-study-finds/"
                .into(),
        ];
        let opts = pick_link_options(&pack, "");
        assert_eq!(opts.len(), 3, "{opts:?}");
        assert_eq!(
            opts[0],
            "https://decrypt.co/376271/chatgpt-web-ai-written-pew"
        );
        assert_eq!(
            opts.iter()
                .filter(|u| u.contains("pewresearch.org"))
                .count(),
            1,
            "{opts:?}"
        );
        assert!(opts.iter().any(|u| u.contains("techcrunch.com")));
    }

    #[test]
    fn propose_digest_forced_cite_is_link_one_with_three_domains() {
        let digest_url = "https://decrypt.co/376271/chatgpt-web-ai-written-pew";
        let pack = vec![
            digest_url.into(),
            "https://www.pewresearch.org/data-labs/2026/08/20/how-much-of-the-internet-is-written-with-ai/"
                .into(),
            "https://www.pewresearch.org/data-labs/2026/08/20/methodology-ai-content/".into(),
            "https://techcrunch.com/2026/08/20/a-third-of-webpages-published-since-chatgpts-launch-show-signs-of-ai-authorship-study-finds/"
                .into(),
        ];
        let link_options = propose_link_options(&pack, "", digest_url);
        assert_three_unique_domains(&link_options);
        assert_eq!(link_options[0], digest_url);
    }

    #[test]
    fn propose_forced_cite_first_even_when_body_prefers_pew() {
        let digest_url = "https://decrypt.co/376271/chatgpt-web-ai-written-pew";
        let pew =
            "https://www.pewresearch.org/data-labs/2026/08/20/how-much-of-the-internet-is-written-with-ai/";
        let pack = vec![
            digest_url.into(),
            pew.into(),
            "https://www.pewresearch.org/data-labs/2026/08/20/methodology-ai-content/".into(),
            "https://techcrunch.com/2026/08/20/a-third-of-webpages-published-since-chatgpts-launch-show-signs-of-ai-authorship-study-finds/"
                .into(),
        ];
        let body = format!("Commentary.\n\n{pew}\n");
        let opts = propose_link_options(&pack, &body, digest_url);
        assert_three_unique_domains(&opts);
        assert_eq!(opts[0], digest_url, "{opts:?}");
        assert_eq!(
            opts.iter()
                .filter(|u| u.contains("pewresearch.org"))
                .count(),
            1
        );
    }

    #[test]
    fn propose_forced_cite_first_when_decrypt_last_in_pack() {
        let digest_url = "https://decrypt.co/376271/chatgpt-web-ai-written-pew";
        let pack = vec![
            "https://www.pewresearch.org/data-labs/2026/08/20/how-much-of-the-internet-is-written-with-ai/"
                .into(),
            "https://techcrunch.com/2026/08/20/a-third-of-webpages-published-since-chatgpts-launch-show-signs-of-ai-authorship-study-finds/"
                .into(),
            "https://www.pewresearch.org/data-labs/2026/08/20/methodology-ai-content/".into(),
            digest_url.into(),
        ];
        let opts = propose_link_options(&pack, "", digest_url);
        assert_three_unique_domains(&opts);
        assert_eq!(opts[0], digest_url);
    }

    #[test]
    fn propose_decrypt_not_dropped_by_t_co_substring_bug() {
        let digest_url = "https://decrypt.co/376271/chatgpt-web-ai-written-pew";
        let pack = vec![
            digest_url.into(),
            "https://www.pewresearch.org/data-labs/2026/08/20/how-much-of-the-internet-is-written-with-ai/"
                .into(),
            "https://www.pewresearch.org/data-labs/2026/08/20/methodology-ai-content/".into(),
            "https://techcrunch.com/2026/08/20/a-third-of-webpages-published-since-chatgpts-launch-show-signs-of-ai-authorship-study-finds/"
                .into(),
        ];
        let raw = pick_link_options(&pack, "");
        assert_eq!(
            raw.len(),
            3,
            "decrypt must survive filter_publisher_urls: {raw:?}"
        );
        assert_eq!(raw[0], digest_url);
        let finalized = propose_link_options(&pack, "", digest_url);
        assert_three_unique_domains(&finalized);
        assert_eq!(finalized[0], digest_url);
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
        let cite = "https://labs.sogeti.com/article";
        let out = ensure_primary_link_line(body, Some(cite));
        assert!(out.contains(cite));
        assert!(out.contains("Commentary here."));
        assert!(
            out.contains("Commentary here.\n\nhttps://"),
            "blank line before cite: {out}"
        );
    }

    #[test]
    fn ensure_normalizes_blank_line_before_existing_cite() {
        let cite =
            "https://blog.rust-lang.org/inside-rust/2026/08/18/reducing-target-dir-size-on-nightly";
        let body = format!(
            "I'm watching how these small changes become habits. 🦉\n{cite}\n\nWritten by AI - ITCy - model ollama/qwen3:8b <in:1 out:1>"
        );
        let out = ensure_primary_link_line(&body, Some(cite));
        assert!(
            out.contains("🦉\n\nhttps://"),
            "blank line before cite: {out}"
        );
        assert!(out.contains("Written by AI"));
        assert_eq!(
            out.lines()
                .filter(|l| l.trim().starts_with("https://"))
                .count(),
            1
        );
    }

    #[test]
    fn ensure_inserts_before_disclosure_with_blank_line() {
        let cite = "https://labs.sogeti.com/article";
        let body =
            "Commentary here.\n\nWritten by AI - ITCy - model ollama/qwen3:8b - tokens in:1 out:1";
        let out = ensure_primary_link_line(body, Some(cite));
        assert!(
            out.contains("Commentary here.\n\nhttps://"),
            "blank line before cite: {out}"
        );
        assert!(out.contains("\n\nWritten by AI"));
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

    #[test]
    fn strip_leading_page_title_lede_drops_openai_cursor_title() {
        // DRAFT-20260829-000133: model pasted browse Page Title as the LinkedIn lede.
        let body = "\
Our decision on Cursor following its acquisition by SpaceX | @openai August 28, 2026

📜 Today, @openai made a move that feels both strategic and symbolic.

https://openai.com/index/our-decision-on-cursor-following-its-acquisition-by-spacex
";
        let out = strip_leading_page_title_lede(body);
        assert!(
            !out.contains("Our decision on Cursor"),
            "title lede must die: {out}"
        );
        assert!(out.starts_with("📜 Today, @openai"), "{out}");
    }

    #[test]
    fn strip_browse_page_title_chrome_drops_a11y_title_line() {
        let snap = "\
### Page
- Page URL: https://openai.com/index/our-decision-on-cursor-following-its-acquisition-by-spacex/
- Page Title: Our decision on Cursor following its acquisition by SpaceX | OpenAI
### Snapshot
body text about November 12 shutoff
";
        let out = strip_browse_page_title_chrome(snap);
        assert!(!out.to_ascii_lowercase().contains("page title:"), "{out}");
        assert!(out.contains("November 12"), "{out}");
    }

    #[test]
    fn strip_rework_quoted_removals_honors_operator_quotes() {
        let body = "\
Our decision on Cursor following its acquisition by SpaceX | @openai August 28, 2026

📜 Today, OpenAI made a move.
";
        let instructions = "remove this what is this crap ?\" Our decision on Cursor following its acquisition by SpaceX | @openai August 28, 2026\" correctly cite 📜 Today, @openai";
        let out = strip_rework_quoted_removals(body, instructions);
        assert!(
            !out.contains("Our decision on Cursor"),
            "quoted removal must apply: {out}"
        );
        assert!(out.contains("📜 Today"), "{out}");
    }

    #[test]
    fn slack_paste_safe_reply_fences_and_expands_owl() {
        let shown = slack_paste_safe_reply_body("Hello :owl: world.");
        assert!(shown.starts_with("```\n"), "{shown}");
        assert!(shown.contains("🦉"), "{shown}");
        assert!(!shown.contains(":owl:"), "{shown}");
        assert!(
            shown.ends_with("\n```") || shown.ends_with("```"),
            "{shown}"
        );
    }

    #[test]
    fn change_url_pipeline_preserves_paragraph_aeration() {
        use crate::sources::draft_url::set_single_in_post_url;
        let prose = "\
Para one about Sätteri and Astro compile wins on content-heavy sites.\n\n\
Para two about native GFM, math, and wikilinks without plugin sprawl.\n\n\
https://byteiota.com/astro-64-satteri-rust-markdown-plugin-tradeoff\n";
        let composed = compose_draft_message(
            prose,
            "DRAFT-20260828-000122",
            &[
                "https://byteiota.com/astro-64-satteri-rust-markdown-plugin-tradeoff".into(),
                "https://www.infoq.com/news/2026/08/astro-satteri-rust".into(),
            ],
        );
        let swapped = set_single_in_post_url(
            &composed,
            "https://www.infoq.com/news/2026/08/astro-satteri-rust",
        );
        let body_only = draft_prose_for_rework(&swapped);
        assert!(
            body_only.contains("Para one about Sätteri")
                && body_only.contains("Para two about native GFM")
                && body_only.contains("\n\n"),
            "/change_url normalize must keep \\n\\n aeration: {body_only:?}"
        );
    }

    #[test]
    fn compose_draft_message_preserves_paragraph_aeration() {
        let prose = "\
Para one about Sätteri and Astro compile wins on content-heavy sites.\n\n\
Para two about native GFM, math, and wikilinks without plugin sprawl.\n\n\
https://byteiota.com/astro-64-satteri-rust-markdown-plugin-tradeoff\n";
        let links = vec![
            "https://byteiota.com/astro-64-satteri-rust-markdown-plugin-tradeoff".into(),
            "https://www.infoq.com/news/2026/08/astro-satteri-rust".into(),
        ];
        let composed = compose_draft_message(prose, "DRAFT-20260828-000122", &links);
        let body_only = draft_prose_for_rework(&composed);
        assert!(
            body_only.contains("Para one about Sätteri"),
            "compose must keep prose: {body_only}"
        );
        assert!(
            body_only.contains("Para one about Sätteri")
                && body_only.contains("Para two about native GFM")
                && body_only.contains("\n\n"),
            "compose + rework extract must keep \\n\\n aeration: {body_only:?}"
        );
    }

    #[test]
    fn aerate_doordash_wall_splits_three_dense_paragraphs() {
        use crate::sources::digest_propose_fixtures::FIXTURE_E_WALL_BODY;
        let out = aerate_linkedin_draft(FIXTURE_E_WALL_BODY);
        assert!(
            out.contains("\n\n"),
            "wall text must gain paragraph breaks: {out:?}"
        );
        assert!(
            out.contains("130K tasks") && out.contains("Firecracker") && out.contains("The future"),
            "all facts must remain: {out}"
        );
        let blocks: Vec<&str> = out.split("\n\n").collect();
        assert_eq!(blocks.len(), 3, "expected hook / body / close: {out:?}");
        assert!(
            blocks[0].contains("audit-ready") && blocks[0].contains("weekly"),
            "opening hook paragraph: {:?}",
            blocks[0]
        );
        assert!(
            blocks[1].contains("Firecracker") && blocks[1].contains("blueprint"),
            "middle development paragraph: {:?}",
            blocks[1]
        );
        assert!(
            blocks[2].contains("The future of engineering"),
            "closing paragraph: {:?}",
            blocks[2]
        );
        for block in &blocks {
            assert!(
                block.split_whitespace().count() > 14,
                "LinkedIn blocks must stay dense, not X one-liners: {block:?}"
            );
        }
    }

    #[test]
    fn aerate_preserves_existing_paragraph_breaks() {
        let already = "Hook paragraph with enough words to stay dense on LinkedIn for builders who read carefully.\n\n\
Second paragraph names the change and why teams care about the tradeoff in production.\n\n\
https://example.com/article\n";
        assert_eq!(aerate_linkedin_draft(already), already.trim());
    }

    #[test]
    fn aerate_two_sentences_becomes_two_paragraphs() {
        let wall = "First sentence names the entity and the concrete change for builders. \
Second sentence develops why maintainers care about the tradeoff this week.";
        let out = aerate_linkedin_draft(wall);
        assert_eq!(out.matches("\n\n").count(), 1, "{out}");
    }

    #[test]
    fn aerate_four_sentences_without_closer_splits_in_half() {
        let wall = "One names the ship and the verb. Two adds context for builders. \
Three develops the tradeoff. Four lands the peer consequence.";
        let out = aerate_linkedin_draft(wall);
        assert!(
            out.contains("\n\n"),
            "four-sentence wall must aerate: {out}"
        );
        assert_eq!(out.split("\n\n").count(), 2, "{out:?}");
    }

    #[test]
    fn aerate_single_sentence_wall_unchanged() {
        let one = "Only one sentence here.";
        assert_eq!(aerate_linkedin_draft(one), one);
    }

    #[test]
    fn split_prose_sentences_keeps_closing_emoji_on_same_sentence() {
        let s = split_prose_sentences(
            "The future of engineering is less about laptops and more about centralized systems. 📜",
        );
        assert_eq!(s.len(), 1, "trailing scroll emoji must not split: {s:?}");
        assert!(s[0].contains("📜"), "{s:?}");
    }

    #[test]
    fn split_prose_sentences_keeps_130k_and_colon_phrase() {
        let s = split_prose_sentences(
            "The numbers speak for themselves: 130K tasks automated in a single month, 25K code reviews weekly. \
Next sentence starts here.",
        );
        assert_eq!(s.len(), 2, "{s:?}");
        assert!(s[0].contains("130K"), "{s:?}");
    }

    #[test]
    fn aerate_opens_new_paragraph_before_context_emoji_sentence() {
        let wall = "Opening hook with enough words for LinkedIn readers who skim feeds quickly. \
Still hooking with a second sentence before the technical turn. \
🔧 Tooling detail sentence names Firecracker and scoped access for builders. \
Closing names why teams care about audit trails in production.";
        let out = aerate_linkedin_draft(wall);
        let blocks: Vec<&str> = out.split("\n\n").collect();
        assert!(
            blocks
                .iter()
                .any(|b| b.contains('🔧') && b.contains("Firecracker")),
            "emoji-led development must stay in a dense block: {out:?}"
        );
    }

    #[test]
    fn linkedin_manual_paste_preserves_paragraph_aeration() {
        let prose = "\
Para one about Sätteri and Astro compile wins.\n\n\
Para two about native GFM without plugin sprawl.\n\n\
https://byteiota.com/astro-64-satteri-rust-markdown-plugin-tradeoff\n";
        let paste = linkedin_manual_paste_message(prose, "ollama/qwen3:8b", 100, 200);
        assert!(
            paste.contains("Para one about Sätteri")
                && paste.contains("Para two about native GFM")
                && paste.contains("\n\n"),
            "LinkedIn paste block must keep paragraph breaks: {paste:?}"
        );
        assert!(
            paste.contains("https://byteiota.com/astro-64-satteri-rust-markdown-plugin-tradeoff"),
            "paste must keep cite URL: {paste}"
        );
    }

    #[test]
    fn slack_paste_safe_fences_prose_keeps_footer() {
        let stored = compose_draft_message(
            "Hello 🦉 world.\n\nhttps://example.com/a\n",
            "DRAFT-20260820-000001",
            &["https://example.com/a".into()],
        );
        let shown = slack_paste_safe_linkedin_message(&stored);
        assert!(shown.contains("```\nHello 🦉 world."));
        assert!(shown.contains(":memo: Draft ID:"), "{shown}");
        assert!(shown.contains("Link: :one:"), "{shown}");
        assert!(!shown.contains(":dart: Link:"), "{shown}");
        assert!(!shown.contains(":one: 1"), "{shown}");
        assert!(
            shown.contains(":one: :dart: https://example.com/a"),
            "{shown}"
        );
        assert!(shown.contains(":zero: 0 = no link"), "{shown}");
        assert!(stored.contains("Link: 1"));
        assert!(!stored.contains(":dart:"), "stored body must stay plain");
        assert!(!stored.contains("```"), "stored body must stay unfenced");
        let prose = draft_prose_for_rework(&shown);
        assert!(prose.contains("Hello 🦉 world."));
        assert!(!prose.contains("```"));
    }

    #[test]
    fn slack_highlight_marks_active_link_and_bullet() {
        let stored = compose_draft_message(
            "Hi.\n\nhttps://example.com/b\n",
            "DRAFT-20260820-000002",
            &[
                "https://example.com/a".into(),
                "https://example.com/b".into(),
            ],
        );
        assert!(stored.contains("Link: 2"));
        let shown = slack_highlight_active_link(&stored);
        assert!(shown.contains("Link: :two:"), "{shown}");
        assert!(!shown.contains(":dart: Link:"), "{shown}");
        assert!(
            shown.contains(":two: :dart: https://example.com/b"),
            "{shown}"
        );
        assert!(shown.contains(":one: https://example.com/a"), "{shown}");
        assert!(!shown.contains(":one: :dart:"), "{shown}");
        assert!(!shown.contains("1. https://"), "{shown}");
        assert!(!shown.contains("2. https://"), "{shown}");
    }

    #[test]
    fn manual_paste_strips_chrome_and_uses_angle_token_counts() {
        let stored = compose_draft_message(
            "The project, rangular, is tiny. 🦀\n\nhttps://github.com/Interchouette-ITC/rangular\n",
            "DRAFT-20260824-000093",
            &[
                "https://github.com/Interchouette-ITC/rangular".into(),
                "https://interchouette.net/news".into(),
            ],
        );
        let with_disc =
            crate::llm::disclosure::ensure_stored_disclosure(&stored, "ollama/qwen3:8b", 3918, 290);
        let paste = linkedin_manual_paste_message(&with_disc, "ollama/qwen3:8b", 3918, 290);
        assert!(paste.contains(":clipboard: LinkedIn paste"));
        assert!(paste.contains("```\n"));
        assert!(paste.contains("rangular"));
        assert!(paste.contains("https://github.com/Interchouette-ITC/rangular"));
        assert!(paste.contains("<in:3918 out:290>"));
        assert!(!paste.contains("Draft ID:"));
        assert!(!paste.contains("Link:"));
        assert!(!paste.contains("0 = no link"));
        assert!(!paste.contains("tokens in:"));
    }

    #[test]
    fn slack_paste_safe_ignores_tweets() {
        let tweet = "Tweet ID: TWEET-1\n\nHello :owl:";
        let shown = slack_paste_safe_linkedin_message(tweet);
        assert!(shown.contains(":bird: Tweet ID: TWEET-1"), "{shown}");
        assert!(shown.contains("Hello :owl:"), "{shown}");
        assert!(!tweet.contains(":bird:"), "stored/plain tweet unchanged");
    }
}

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

/// Drop a leading `cite` instruction leak (`cite 📜 @handle…`, `Cite: https://…`).
///
/// Operator briefs use `, cite https://…`; the writer must not open the post with that word.
#[must_use]
pub fn strip_leading_cite_instruction(body: &str) -> String {
    let trimmed = body.trim_start();
    let Some(rest_after_cite) = strip_cite_prefix(trimmed) else {
        return body.to_string();
    };
    let rest = rest_after_cite.trim_start_matches([':', ',', '-', ' ']);
    let rest = rest.trim_start();
    if !leading_cite_looks_like_meta(rest) {
        return body.to_string();
    }
    rest.to_string()
}

fn strip_cite_prefix(s: &str) -> Option<&str> {
    let mut chars = s.chars();
    let first = chars.next()?;
    if !first.eq_ignore_ascii_case(&'c') {
        return None;
    }
    let second = chars.next()?;
    if !second.eq_ignore_ascii_case(&'i') {
        return None;
    }
    let third = chars.next()?;
    if !third.eq_ignore_ascii_case(&'t') {
        return None;
    }
    let fourth = chars.next()?;
    if !fourth.eq_ignore_ascii_case(&'e') {
        return None;
    }
    let rest = chars.as_str();
    match rest.chars().next() {
        None => Some(""),
        Some(c) if c.is_alphanumeric() => None, // cited / citation / …
        Some(_) => Some(rest),
    }
}

fn leading_cite_looks_like_meta(rest: &str) -> bool {
    let t = rest.trim_start();
    if t.is_empty() {
        return false;
    }
    if t.starts_with('@') || t.starts_with("http://") || t.starts_with("https://") {
        return true;
    }
    t.chars()
        .next()
        .is_some_and(|c| !c.is_ascii_alphanumeric() && c != '#' && !c.is_whitespace())
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

/// Phrases the operator wants rewritten (must not remain verbatim after `/rework`).
///
/// Includes quoted spans and prior-body sentences the operator pasted into instructions
/// when asking to reformulate / rewrite / not copy. For a full replacement draft, bans
/// prior sentences that are absent from the new draft.
#[must_use]
pub fn rework_verbatim_ban_phrases(prior: &str, instructions: &str) -> Vec<String> {
    let mut out = Vec::new();
    for phrase in quoted_phrases_in_instructions(instructions) {
        if phrase.chars().count() >= 24 {
            push_ban_unique(&mut out, phrase);
        }
    }
    if rework_looks_like_replacement_draft(instructions) {
        for sentence in prose_sentences(prior) {
            if sentence.chars().count() < 40 {
                continue;
            }
            if !instructions_echoes_span(instructions, &sentence) {
                push_ban_unique(&mut out, sentence);
            }
        }
        return out;
    }
    let ask = instructions_ask_rewrite(instructions);
    if ask {
        for sentence in prose_sentences(prior) {
            if sentence.chars().count() < 40 {
                continue;
            }
            if instructions_echoes_span(instructions, &sentence) {
                push_ban_unique(&mut out, sentence);
            }
        }
        if out.is_empty() {
            for sentence in last_n_sentences(prior, rewrite_sentence_count(instructions)) {
                if sentence.chars().count() >= 24 {
                    push_ban_unique(&mut out, sentence);
                }
            }
        }
    }
    out
}

/// `/rework` input mode for replies, drafts, and tweets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReworkMode {
    /// `/rework <id>` with no text: redo from subject + pack.
    Refresh,
    /// Short/medium edit directives (reformulate, lengthen, …).
    Instruction,
    /// Operator pasted a full replacement body (`use text` or long paste).
    Replace,
}

/// If instructions start with `use text` (optional `:`), return the body after the prefix.
#[must_use]
pub fn strip_use_text_prefix(instructions: &str) -> Option<&str> {
    const PREFIX: &str = "use text";
    let t = instructions.trim();
    let lower = t.to_ascii_lowercase();
    if !lower.starts_with(PREFIX) {
        return None;
    }
    let rest = t.get(PREFIX.len()..)?.trim_start();
    Some(rest.strip_prefix(':').map_or(rest, str::trim_start))
}

/// Body to save on Replace: strip `use text` when present, else the full paste.
#[must_use]
pub fn rework_replacement_body(instructions: &str) -> &str {
    let t = instructions.trim();
    strip_use_text_prefix(t).unwrap_or(t)
}

/// Classify `/rework` text into refresh / instruction / replace.
#[must_use]
pub fn classify_rework_mode(instructions: &str) -> ReworkMode {
    let t = instructions.trim();
    if t.is_empty() {
        return ReworkMode::Refresh;
    }
    if strip_use_text_prefix(t).is_some() {
        return ReworkMode::Replace;
    }
    if rework_looks_like_replacement_draft(t) {
        return ReworkMode::Replace;
    }
    ReworkMode::Instruction
}

/// `replace FROM to|with TO` pairs from operator instructions (hard keyword, like `cite` / `quote`).
#[must_use]
pub fn extract_rework_replaces(instructions: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let lower = instructions.to_ascii_lowercase();
    let mut search_from = 0_usize;
    while search_from < lower.len() {
        let Some(rel) = lower[search_from..].find("replace") else {
            break;
        };
        let start = search_from + rel;
        if !keyword_boundary_before(instructions, start)
            || !keyword_boundary_after(instructions, start + "replace".len())
        {
            search_from = start + 7;
            continue;
        }
        let mut rest = instructions[start + 7..].trim_start();
        rest = rest.strip_prefix(':').map_or(rest, str::trim_start);
        let Some((from, after_from)) = take_directive_span(rest) else {
            search_from = start + 7;
            continue;
        };
        let after_from = after_from.trim_start();
        let after_sep = if let Some(r) = strip_prefix_ci(after_from, "to ") {
            r
        } else if let Some(r) = strip_prefix_ci(after_from, "with ") {
            r
        } else if let Some(r) = after_from.strip_prefix("->") {
            r.trim_start()
        } else {
            search_from = start + 7;
            continue;
        };
        let Some((to, _)) = take_directive_span(after_sep) else {
            search_from = start + 7;
            continue;
        };
        let from = strip_wrapping_quotes(from).trim().to_string();
        let to = strip_wrapping_quotes(to).trim().to_string();
        if from.chars().count() >= 2 && !to.is_empty() && from != to {
            out.push((from, to));
        }
        search_from = start + 7;
    }
    out
}

/// `"Name" is handle @slug` maps (hard; do not leave to the model).
#[must_use]
pub fn extract_rework_handle_maps(instructions: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let lower = instructions.to_ascii_lowercase();
    let mut search_from = 0_usize;
    while search_from < lower.len() {
        let Some(rel) = lower[search_from..].find(" is handle ") else {
            break;
        };
        let mid = search_from + rel;
        let before = instructions[..mid].trim_end();
        let after = instructions[mid + " is handle ".len()..].trim_start();
        let Some(name) = take_directive_span_ending_at(before) else {
            search_from = mid + 1;
            continue;
        };
        let Some((handle, _)) = take_directive_span(after) else {
            search_from = mid + 1;
            continue;
        };
        let name = strip_wrapping_quotes(name).trim().to_string();
        let mut handle = strip_wrapping_quotes(handle).trim().to_string();
        if !handle.starts_with('@') {
            handle = format!("@{handle}");
        }
        if name.chars().count() >= 2 && handle.len() >= 2 {
            out.push((name, handle));
        }
        search_from = mid + " is handle ".len();
    }
    out
}

/// True when instructions are only `replace` / `is handle` edits (connectors OK).
#[must_use]
pub fn rework_instructions_are_keyword_edits_only(instructions: &str) -> bool {
    let replaces = extract_rework_replaces(instructions);
    let handles = extract_rework_handle_maps(instructions);
    if replaces.is_empty() && handles.is_empty() {
        return false;
    }
    let mut known: Vec<String> = vec![
        "replace".into(),
        "to".into(),
        "with".into(),
        "is".into(),
        "handle".into(),
        "however".into(),
        "and".into(),
        "also".into(),
        "then".into(),
        "please".into(),
        "but".into(),
    ];
    for (from, to) in &replaces {
        known.push(from.to_ascii_lowercase());
        known.push(to.to_ascii_lowercase());
    }
    for (name, handle) in &handles {
        known.push(name.to_ascii_lowercase());
        known.push(handle.to_ascii_lowercase());
        known.push(handle.trim_start_matches('@').to_ascii_lowercase());
        for part in name.split_whitespace() {
            known.push(part.to_ascii_lowercase());
        }
    }
    instructions
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '@')
        .filter(|w| !w.is_empty())
        .all(|w| known.iter().any(|k| k == &w.to_ascii_lowercase()))
}

/// Apply operator `replace` + `is handle` keyword edits to prose (hard).
#[must_use]
pub fn apply_rework_keyword_edits(body: &str, instructions: &str) -> String {
    let mut out = body.to_string();
    for (from, to) in extract_rework_replaces(instructions) {
        out = out.replace(&from, &to);
    }
    for (name, handle) in extract_rework_handle_maps(instructions) {
        out = out.replace(&name, &handle);
    }
    out
}

/// Missing `replace` outcomes: `from` still present, or `to` never landed when `from` was in prior.
#[must_use]
pub fn missing_rework_replace_outcomes(prior: &str, body: &str, instructions: &str) -> Vec<String> {
    let mut missing = Vec::new();
    for (from, to) in extract_rework_replaces(instructions) {
        if body.contains(&from) {
            missing.push(format!("still contains `{from}` (replace with `{to}`)"));
        } else if prior.contains(&from) && !body.contains(&to) {
            missing.push(format!("missing `{to}` after replacing `{from}`"));
        }
    }
    for (name, handle) in extract_rework_handle_maps(instructions) {
        if body.contains(&name) && !body.contains(&handle) {
            missing.push(format!("`{name}` must become `{handle}`"));
        } else if prior.contains(&name) && !body.contains(&handle) && !body.contains(&name) {
            missing.push(format!("missing handle `{handle}` for `{name}`"));
        }
    }
    missing
}

fn keyword_boundary_before(text: &str, start: usize) -> bool {
    if start == 0 {
        return true;
    }
    let Some(prev) = text[..start].chars().next_back() else {
        return true;
    };
    prev.is_whitespace() || matches!(prev, ',' | ';' | ':' | '|' | '(' | '[')
}

const fn keyword_boundary_after(text: &str, after: usize) -> bool {
    if after >= text.len() {
        return true;
    }
    !text.as_bytes()[after].is_ascii_alphanumeric()
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn take_directive_span(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix('"') {
        let end = rest.find('"')?;
        let val = &rest[..end];
        let after = &rest[end + 1..];
        return Some((val, after));
    }
    if s.starts_with('\u{201c}') {
        let end = s['\u{201c}'.len_utf8()..].find('\u{201d}')?;
        let val = &s['\u{201c}'.len_utf8()..'\u{201c}'.len_utf8() + end];
        let rest = &s['\u{201c}'.len_utf8() + end + '\u{201d}'.len_utf8()..];
        return Some((val, rest));
    }
    let end = s
        .find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ')' | ']'))
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    Some((&s[..end], &s[end..]))
}

/// Last quoted span or trailing tokens at the end of `before` (for `… Name is handle`).
fn take_directive_span_ending_at(before: &str) -> Option<&str> {
    let t = before.trim_end();
    if let Some(inner) = t.strip_suffix('"') {
        let start = inner.rfind('"')?;
        return Some(inner[start + 1..].trim());
    }
    // Unquoted multi-word name: take from last connector / start.
    let start = t.rfind([',', ';', '.', '|', '(']).map_or(0, |i| i + 1);
    let name = t[start..].trim();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Operator `quote` keyword values from a brief (draft / tweet / rework).
///
/// Same family as `cite https://…`: `quote Ship the App, not the Plumbing.` or
/// `quote: Ship the App, not the Plumbing.` Value runs until the next `cite` /
/// `quote` keyword, an `https://` URL, or end of brief.
#[must_use]
pub fn extract_brief_quotes(brief: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = brief.to_ascii_lowercase();
    let bytes = brief.as_bytes();
    let mut search_from = 0_usize;
    while search_from < lower.len() {
        let Some(rel) = lower[search_from..].find("quote") else {
            break;
        };
        let start = search_from + rel;
        if !quote_keyword_boundary_before(brief, start) {
            search_from = start + 5;
            continue;
        }
        let after_kw = start + 5;
        if after_kw < bytes.len() && bytes[after_kw].is_ascii_alphanumeric() {
            search_from = after_kw;
            continue;
        }
        let mut value_start = after_kw;
        while value_start < bytes.len()
            && (bytes[value_start] == b':' || bytes[value_start].is_ascii_whitespace())
        {
            value_start += 1;
        }
        let value_end = quote_value_end(brief, &lower, value_start);
        if value_end <= value_start {
            search_from = after_kw;
            continue;
        }
        let raw = brief[value_start..value_end].trim();
        let cleaned = strip_wrapping_quotes(raw)
            .trim_matches(|c: char| c == ',' || c.is_whitespace())
            .trim()
            .to_string();
        if cleaned.chars().count() >= 3 {
            push_ban_unique(&mut out, cleaned);
        }
        search_from = value_end;
    }
    out
}

fn quote_keyword_boundary_before(brief: &str, start: usize) -> bool {
    if start == 0 {
        return true;
    }
    let Some(prev) = brief[..start].chars().next_back() else {
        return true;
    };
    prev.is_whitespace() || matches!(prev, ',' | ';' | ':' | '|' | '(' | '[')
}

fn quote_value_end(brief: &str, lower: &str, value_start: usize) -> usize {
    let rest = &lower[value_start..];
    let mut end = brief.len();
    for key in ["cite", "quote"] {
        let mut from = 0_usize;
        while let Some(rel) = rest[from..].find(key) {
            let at = value_start + from + rel;
            if at == value_start && key == "quote" {
                from += rel + key.len();
                continue;
            }
            if quote_keyword_boundary_before(brief, at) {
                let after = at + key.len();
                if after >= brief.len() || !brief.as_bytes()[after].is_ascii_alphanumeric() {
                    end = end.min(at);
                    break;
                }
            }
            from += rel + key.len();
        }
    }
    if let Some(rel) = rest.find("https://") {
        end = end.min(value_start + rel);
    }
    if let Some(rel) = rest.find("http://") {
        end = end.min(value_start + rel);
    }
    while end > value_start {
        let ch = brief[..end].chars().next_back().unwrap_or('\0');
        if ch.is_whitespace() || ch == ',' || ch == ';' {
            end -= ch.len_utf8();
        } else {
            break;
        }
    }
    end
}

fn strip_wrapping_quotes(s: &str) -> &str {
    let t = s.trim();
    for (open, close) in [
        ('"', '"'),
        ('\u{201c}', '\u{201d}'),
        ('\u{2018}', '\u{2019}'),
        ('\'', '\''),
    ] {
        if t.starts_with(open)
            && t.ends_with(close)
            && t.len() >= open.len_utf8() + close.len_utf8()
        {
            return t[open.len_utf8()..t.len() - close.len_utf8()].trim();
        }
    }
    t
}

/// Required slogan / phrase spans from the operator `quote` keyword.
#[must_use]
pub fn rework_required_quoted_spans(instructions: &str) -> Vec<String> {
    extract_brief_quotes(instructions)
}

/// Required `quote` values that are missing from `body`.
#[must_use]
pub fn missing_required_quoted_spans(body: &str, required: &[String]) -> Vec<String> {
    required
        .iter()
        .filter(|q| !body_contains_required_quote(body, q))
        .cloned()
        .collect()
}

fn body_contains_required_quote(body: &str, slogan: &str) -> bool {
    if body.contains(slogan) {
        return true;
    }
    for (open, close) in [('"', '"'), ('\u{201c}', '\u{201d}')] {
        let wrapped = format!("{open}{slogan}{close}");
        if body.contains(&wrapped) {
            return true;
        }
    }
    false
}

/// First-pass pack note when the brief has `quote …` values.
#[must_use]
pub fn operator_quote_pack_note(brief: &str) -> Option<String> {
    let quotes = extract_brief_quotes(brief);
    if quotes.is_empty() {
        return None;
    }
    let mut s = String::from(
        "OPERATOR quote keyword (hard): include each phrase verbatim with double quotes around it:\n",
    );
    for q in quotes {
        s.push_str("- \"");
        s.push_str(&q);
        s.push_str("\"\n");
    }
    Some(s)
}

/// Louder pack-note / instruction suffix when required `quote` values were missing.
#[must_use]
pub fn louder_required_quotes_note(missing: &[String]) -> String {
    let mut s = String::from(
        "HARD: previous draft missed operator `quote` text. Include each phrase verbatim (with double quotes around it):\n",
    );
    for q in missing {
        s.push_str("- \"");
        s.push_str(q);
        s.push_str("\"\n");
    }
    s
}

/// Operator-facing error when required `quote` values never landed.
#[must_use]
pub fn missing_quotes_operator_error(missing: &[String]) -> String {
    format!(
        "writer did not include required quote text: {}",
        missing
            .iter()
            .map(|q| format!("\"{q}\""))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Phrases to avoid on empty `/rework` refresh (prior body beats / sentences).
#[must_use]
pub fn rework_refresh_ban_phrases(prior: &str) -> Vec<String> {
    let mut out = Vec::new();
    for sentence in prose_sentences(prior) {
        if sentence.chars().count() >= 24 {
            push_ban_unique(&mut out, sentence);
        }
    }
    for line in prior.lines() {
        let t = line.trim();
        if t.chars().count() >= 24 {
            push_ban_unique(&mut out, t.to_string());
        }
    }
    out
}

/// True when `/rework` text is a full replacement draft (not a short polish tip).
#[must_use]
pub fn rework_looks_like_replacement_draft(instructions: &str) -> bool {
    let t = instructions.trim();
    if t.chars().count() < 160 {
        return false;
    }
    if t.split_whitespace().count() < 40 {
        return false;
    }
    if prose_sentences(t).len() < 2 {
        return false;
    }
    if instructions_are_edit_directives(t) {
        return false;
    }
    true
}

/// True when rework output collapsed into a cheer stub vs a longer prior.
#[must_use]
pub fn rework_collapsed_too_much(prior: &str, next: &str, instructions: &str) -> bool {
    if instructions_allow_shorten(instructions) {
        return false;
    }
    let prior_n = prior.split_whitespace().count();
    let next_n = next.split_whitespace().count();
    if prior_n < 24 {
        return false;
    }
    let floor = (prior_n * 2 / 5).max(12);
    next_n < floor
}

fn instructions_allow_shorten(instructions: &str) -> bool {
    let l = instructions.to_ascii_lowercase();
    l.contains("shorter")
        || l.contains("shorten")
        || l.contains("one sentence")
        || l.contains("1 sentence")
        || l.contains("brief")
        || l.contains("tighter")
}

fn instructions_are_edit_directives(instructions: &str) -> bool {
    const HEADS: &[&str] = &[
        "make ",
        "rewrite",
        "rephrase",
        "reformulat",
        "refomulat",
        "refourmlat",
        "do not ",
        "don't ",
        "dont ",
        "add ",
        "remove ",
        "delete ",
        "lengthen",
        "shorten",
        "expand ",
        "fix ",
        "change ",
        "keep ",
        "drop ",
        "replace ",
    ];
    let l = instructions.to_ascii_lowercase();
    let head = l.lines().next().unwrap_or("").trim();
    if HEADS.iter().any(|h| head.starts_with(h)) {
        return true;
    }
    if instructions_ask_rewrite(instructions) {
        let first = prose_sentences(instructions)
            .into_iter()
            .next()
            .unwrap_or_default();
        if first.chars().count() < 100 {
            return true;
        }
    }
    false
}

/// True when `body` still contains any banned phrase (whitespace-normalized).
#[must_use]
pub fn body_copies_rework_ban(body: &str, banned: &[String]) -> bool {
    let norm_body = collapse_ws(body);
    banned.iter().any(|p| {
        let n = collapse_ws(p);
        n.chars().count() >= 24 && norm_body.contains(&n)
    })
}

fn push_ban_unique(out: &mut Vec<String>, phrase: String) {
    let n = collapse_ws(&phrase);
    if out.iter().any(|e| collapse_ws(e) == n) {
        return;
    }
    out.push(phrase);
}

fn instructions_ask_rewrite(instructions: &str) -> bool {
    let l = instructions.to_ascii_lowercase();
    l.contains("reformulat")
        || l.contains("refomulat")
        || l.contains("refourmlat")
        || l.contains("rephrase")
        || l.contains("rewrite")
        || l.contains("do not copy")
        || l.contains("don't copy")
        || l.contains("dont copy")
        || l.contains("not copy")
}

fn rewrite_sentence_count(instructions: &str) -> usize {
    let l = instructions.to_ascii_lowercase();
    if l.contains("last two") || l.contains("last 2") {
        return 2;
    }
    if l.contains("last three") || l.contains("last 3") {
        return 3;
    }
    if l.contains("last sentence") || l.contains("that sentence") || l.contains("this sentence") {
        return 1;
    }
    0
}

fn last_n_sentences(prior: &str, n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let all = prose_sentences(prior);
    all.into_iter().rev().take(n).rev().collect()
}

fn prose_sentences(prior: &str) -> Vec<String> {
    let text = prior
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("Written by AI"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = Vec::new();
    let mut cur = String::new();
    for (i, ch) in text.char_indices() {
        cur.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            let next = text[i + ch.len_utf8()..].chars().next();
            if next.is_none_or(char::is_whitespace) {
                let s = cur.trim().to_string();
                if !s.is_empty() {
                    out.push(s);
                }
                cur.clear();
            }
        }
    }
    let tail = cur.trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

fn instructions_echoes_span(instructions: &str, span: &str) -> bool {
    collapse_ws(instructions).contains(&collapse_ws(span))
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
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
    fn strip_leading_cite_instruction_drops_brief_leak() {
        // DRAFT-20260902-000146: writer opened with operator "cite …" vocabulary.
        let body = "cite 📜 @seggwat is a tool that's built for SaaS teams.\n\n🦉 Builders triage feedback without leaving the workflow.";
        let out = strip_leading_cite_instruction(body);
        assert!(!out.to_ascii_lowercase().starts_with("cite"), "{out}");
        assert!(out.starts_with("📜 @seggwat"), "{out}");
        assert_eq!(
            strip_leading_cite_instruction("Cited research shows Rust wins."),
            "Cited research shows Rust wins."
        );
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
    fn rework_ban_phrases_catch_pasted_sentences_to_reformulate() {
        // CREPLY-20260902-000006: operator pasted the sentences; model kept copying them.
        let prior = "\
This is exactly the kind of innovation that shifts the paradigm. Lightweight, secure, and powerful. 🦉 \
Databricks bought ElectricSQL, the team behind PGlite, and for agent sandboxes, PGlite makes sense. \
The real unlock is giving an agent a DB it can break and reset in a second instead of pointing it near prod. \
The wasm build was never the hard part though, sync conflict resolution is. Guessing that's the actual asset.";
        let instructions = "\
do not copy reformulate that part The real unlock is giving an agent a DB it can break and reset in a second \
instead of pointing it near prod. The wasm build was never the hard part though, sync conflict resolution is. \
Guessing that's the actual asset.";
        let banned = rework_verbatim_ban_phrases(prior, instructions);
        assert!(
            banned.iter().any(|p| p.contains("real unlock")),
            "must ban pasted unlock sentence: {banned:?}"
        );
        assert!(
            banned.iter().any(|p| p.contains("wasm build")),
            "must ban pasted wasm sentence: {banned:?}"
        );
        let copied = prior;
        assert!(
            body_copies_rework_ban(copied, &banned),
            "unchanged body must fail ban check"
        );
        let rewritten = "\
This is exactly the kind of innovation that shifts the paradigm. Lightweight, secure, and powerful. 🦉 \
Databricks bought ElectricSQL, the team behind PGlite, and for agent sandboxes, PGlite makes sense. \
What matters is a disposable database the agent can trash and reboot instantly, not a path toward prod. \
Wasm packaging was the easy win; conflict-aware sync is the prize.";
        assert!(
            !body_copies_rework_ban(rewritten, &banned),
            "reformulated body must pass: {rewritten}"
        );
    }

    #[test]
    fn rework_ban_phrases_last_two_sentences_when_asked() {
        let prior = "\
Hook line stays about Wasmer and sandboxes for agents in production talk. \
Middle beat names ElectricSQL without changing. \
Last one changes because disposable databases beat prod-adjacent guesses. \
And the closing too when sync conflict resolution is the real asset.";
        let instructions = "reformulate last two sentences";
        let banned = rework_verbatim_ban_phrases(prior, instructions);
        assert!(
            banned.iter().any(|p| p.contains("Last one changes")),
            "{banned:?}"
        );
        assert!(
            banned.iter().any(|p| p.contains("closing too")),
            "{banned:?}"
        );
    }

    #[test]
    fn replacement_draft_bans_old_prior_only_sentences() {
        let prior = "\
This is exactly the kind of innovation that shifts the paradigm. Lightweight, secure, and powerful. 🦉 \
Databricks bought ElectricSQL, the team behind PGlite, and for agent sandboxes, PGlite makes sense. \
The real unlock is giving an agent a DB it can break and reset in a second instead of pointing it near prod. \
The wasm build was never the hard part though, sync conflict resolution is. Guessing that's the actual asset.";
        let instructions = "\
This is exactly the kind of innovation that shifts the paradigm. Lightweight, secure, and powerful. 🦉 \
Databricks acquired ElectricSQL, the team behind PGlite, and for agent sandboxes, PGlite makes a lot of sense. \
PGlite is essentially PostgreSQL compiled to WebAssembly using Emscripten, with a JS/TS interface for embedding it into applications. \
The interesting distinction with Wasmer is that Wasmer is going one level lower: rather than being a PostgreSQL-specific embedded database, \
Wasmer provides a WASM/WASIX runtime that can run PostgreSQL alongside Python, Node.js, PHP, and other workloads. \
So Wasmer could theoretically host the same kind of PostgreSQL WASM workload from a Rust application, while PGlite provides the PostgreSQL-specific embedded experience. \
That distinction between embedded PostgreSQL and a runtime for embedding arbitrary software is what makes Wasmer's approach particularly interesting.";
        assert!(rework_looks_like_replacement_draft(instructions));
        let banned = rework_verbatim_ban_phrases(prior, instructions);
        assert!(
            banned.iter().any(|p| p.contains("real unlock")),
            "old unlock line must be banned: {banned:?}"
        );
        assert!(
            !body_copies_rework_ban(instructions, &banned),
            "operator replacement must not contain banned prior lines"
        );
        assert!(
            body_copies_rework_ban(prior, &banned),
            "stale prior must fail ban check"
        );
    }

    #[test]
    fn classify_rework_modes_refresh_instruction_replace() {
        assert_eq!(classify_rework_mode(""), ReworkMode::Refresh);
        assert_eq!(
            classify_rework_mode("reformulate last two sentences"),
            ReworkMode::Instruction
        );
        assert_eq!(
            classify_rework_mode(
                "do not copy reformulate that part The real unlock is giving an agent a DB it can break and reset in a second instead of pointing it near prod. The wasm build was never the hard part though, sync conflict resolution is. Guessing that's the actual asset."
            ),
            ReworkMode::Instruction
        );
        let replace = "\
This is exactly the kind of innovation that shifts the paradigm. Lightweight, secure, and powerful. 🦉 \
Databricks acquired ElectricSQL, the team behind PGlite, and for agent sandboxes, PGlite makes a lot of sense. \
PGlite is essentially PostgreSQL compiled to WebAssembly using Emscripten, with a JS/TS interface for embedding it into applications. \
The interesting distinction with Wasmer is that Wasmer is going one level lower: rather than being a PostgreSQL-specific embedded database, \
Wasmer provides a WASM/WASIX runtime that can run PostgreSQL alongside Python, Node.js, PHP, and other workloads. \
So Wasmer could theoretically host the same kind of PostgreSQL WASM workload from a Rust application, while PGlite provides the PostgreSQL-specific embedded experience. \
That distinction between embedded PostgreSQL and a runtime for embedding arbitrary software is what makes Wasmer's approach particularly interesting.";
        assert_eq!(classify_rework_mode(replace), ReworkMode::Replace);
        assert_eq!(
            classify_rework_mode("use text\nAutumn ships. \"Ship the App, not the Plumbing.\" 🦉"),
            ReworkMode::Replace
        );
        assert_eq!(
            classify_rework_mode("USE TEXT: short body ok"),
            ReworkMode::Replace
        );
        assert_eq!(
            rework_replacement_body("use text\nHello owl 🦉"),
            "Hello owl 🦉"
        );
        assert_eq!(rework_replacement_body("USE TEXT: Hello"), "Hello");
        let required = rework_required_quoted_spans(
            "Autumn Rust web framework, quote Ship the App, not the Plumbing. Cite https://autumn-web.app/",
        );
        assert!(
            required
                .iter()
                .any(|s| s == "Ship the App, not the Plumbing."),
            "{required:?}"
        );
        assert_eq!(
            extract_brief_quotes("topic, quote: Hello world. cite https://example.com/a"),
            vec!["Hello world.".to_string()]
        );
        assert!(missing_required_quoted_spans("no slogan here", &required)
            .iter()
            .any(|s| s.contains("Ship the App")));
        assert!(missing_required_quoted_spans(
            "We say \"Ship the App, not the Plumbing.\" today",
            &required
        )
        .is_empty());
        assert!(missing_quotes_operator_error(&required).contains("Ship the App"));
        // Bare \"…\" without the quote keyword is not required.
        assert!(rework_required_quoted_spans(
            "include slogan exactly as \"Ship the App, not the Plumbing.\" with double quotes"
        )
        .is_empty());
        let prior_long = "word ".repeat(40);
        let stub = "Cheers, great point 🦉";
        assert!(rework_collapsed_too_much(
            &prior_long,
            stub,
            "make it better"
        ));
        assert!(!rework_collapsed_too_much(
            &prior_long,
            stub,
            "make it shorter"
        ));
    }

    #[test]
    fn rework_replace_and_handle_keywords_from_operator_instruction() {
        let instr = "Replace @wasmerio to \"Wasmer\" however \"Bytecode Alliance\" is handle @bytecodealliance";
        assert_eq!(
            extract_rework_replaces(instr),
            vec![("@wasmerio".into(), "Wasmer".into())]
        );
        assert_eq!(
            extract_rework_handle_maps(instr),
            vec![("Bytecode Alliance".into(), "@bytecodealliance".into())]
        );
        assert!(rework_instructions_are_keyword_edits_only(instr));
        let prior = "Built by the Bytecode Alliance, powering Wasmtime and @wasmerio with LLVM-rival speed.";
        let out = apply_rework_keyword_edits(prior, instr);
        assert!(
            out.contains("Wasmer") && !out.contains("@wasmerio"),
            "replace must land: {out}"
        );
        assert!(
            out.contains("@bytecodealliance") && !out.contains("Bytecode Alliance"),
            "handle map must land: {out}"
        );
        assert!(missing_rework_replace_outcomes(prior, &out, instr).is_empty());
        // LLM-ignored body still fails the outcome check.
        let ignored = prior.to_string();
        let miss = missing_rework_replace_outcomes(prior, &ignored, instr);
        assert!(miss.iter().any(|m| m.contains("@wasmerio")), "{miss:?}");
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

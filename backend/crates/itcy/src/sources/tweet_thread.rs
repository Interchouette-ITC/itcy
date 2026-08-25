// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Ship-time X length: one tweet when the approved body fits 280 weighted
//! characters; otherwise a reply with leftover commentary, hashtags, and the
//! publisher URL.
//!
//! Writer prompts aim for a ~50/50 mix (one post vs root + reply).

use crate::sources::url_hygiene::is_allowed_tweet_cite;

/// X weighted-length cap (t.co URLs count as 23).
pub const X_CHAR_LIMIT: usize = 280;
const TCO_LEN: usize = 23;
const THREAD_MARK: &str = "--- thread ---";

/// Twitter-text v3 weighted length: latin ≈ 1, emoji/CJK ≈ 2, each URL = 23.
#[must_use]
pub fn x_weighted_len(text: &str) -> usize {
    let mut n = 0;
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(end) = url_end_at(rest, 0) {
            n += TCO_LEN;
            rest = &rest[end..];
            continue;
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        n += x_cp_weight(ch as u32);
        rest = &rest[ch.len_utf8()..];
    }
    n
}

#[must_use]
pub fn fits_x_limit(text: &str) -> bool {
    x_weighted_len(text) <= X_CHAR_LIMIT
}

/// Root = forward from the top (paragraphs first, word-split within a beat when needed), up to 280.
///
/// Reply = everything left + tags + URL, unchanged. Root is grown until reply fits;
/// no word-trim on reply (that caused the dangling "But if you're building on" cut).
/// Never invent a self-reply that is only leftover prose with no tags/URL.
#[must_use]
pub fn layout_x_thread(text: &str) -> Vec<String> {
    let text = strip_thread_marks(text);
    if text.is_empty() {
        return Vec::new();
    }
    let peeled = peel_trailer(&text);
    let one = join_head_trailer(&peeled.head, &peeled.trailer);
    if fits_x_limit(&one) {
        return vec![one];
    }
    // No tags/URL trailer: keep one root by dropping whole trailing beats (never mid-sentence first).
    if peeled.trailer.is_empty() {
        return vec![fit_by_dropping_beats(&peeled.head)];
    }
    forward_fill_split(&peeled.head, &peeled.trailer)
}

fn forward_fill_split(head: &str, trailer: &str) -> Vec<String> {
    let (mut root, mut leftover) = take_beats_fitting(head, X_CHAR_LIMIT);
    grow_root_until_reply_fits(&mut root, &mut leftover, trailer);
    rebalance_dangling_root_tail(&mut root, &mut leftover, trailer);
    peel_incomplete_root_tail(&mut root, &mut leftover, trailer);
    let reply = pack_tweet_two(&leftover, trailer);
    let mut out = Vec::new();
    if !root.is_empty() {
        out.push(root);
    }
    if !reply.is_empty() {
        out.push(reply);
    }
    if out.is_empty() {
        vec![trim_to_limit(head, X_CHAR_LIMIT)]
    } else {
        out
    }
}

/// Move words from leftover onto root while root has room and reply+trailer would overflow.
///
/// Word-by-word only while root is mid-sentence. After `.` / `?` / `!`, pull the next sentence
/// (or word-split inside that sentence) instead of dribbling bare words across the boundary.
fn grow_root_until_reply_fits(root: &mut String, leftover: &mut String, trailer: &str) {
    while !leftover.trim().is_empty() && !reply_fits(leftover, trailer) {
        if !root.is_empty() && !fits_x_limit(root) {
            break;
        }
        if root_ends_sentence(root) {
            let (sentence, rest) = take_leading_sentence(leftover);
            if sentence.is_empty() {
                break;
            }
            if try_grow_root_with_chunk(root, leftover, &sentence, &rest) {
                continue;
            }
            break;
        }
        let (word, rest) = take_first_word(leftover);
        if word.is_empty() {
            break;
        }
        if is_dangling_root_tail_word(&word) {
            break;
        }
        let grown = append_word(root, &word);
        if !fits_x_limit(&grown) {
            break;
        }
        *root = grown;
        *leftover = rest;
    }
}

fn try_grow_root_with_chunk(
    root: &mut String,
    leftover: &mut String,
    chunk: &str,
    rest_after_chunk: &str,
) -> bool {
    let grown = append_word(root, chunk);
    if fits_x_limit(&grown) {
        *root = grown;
        *leftover = rest_after_chunk.to_string();
        return true;
    }
    let cap = remaining_root_capacity(root);
    let (piece, chunk_rest) = split_word_prefix(chunk, cap);
    if piece.is_empty() {
        return false;
    }
    *root = append_word(root, &piece);
    *leftover = join_rest(chunk_rest, &[rest_after_chunk.to_string()]);
    true
}

fn remaining_root_capacity(root: &str) -> usize {
    if root.trim().is_empty() {
        return X_CHAR_LIMIT;
    }
    x_weighted_len(root)
        .checked_add(1)
        .and_then(|used| X_CHAR_LIMIT.checked_sub(used))
        .unwrap_or(0)
}

fn root_ends_sentence(root: &str) -> bool {
    let mut chars: Vec<char> = root.trim_end().chars().collect();
    while chars
        .last()
        .is_some_and(|c| c.is_whitespace() || is_emoji_or_symbol_char(*c))
    {
        chars.pop();
    }
    chars.last().is_some_and(|c| matches!(c, '.' | '!' | '?'))
}

/// Move a trailing mid-sentence fragment off the root when a prior sentence end exists.
///
/// `take_beats_fitting` may word-fill the next beat into leftover root capacity
/// (`…project. 🚀` + `Cargo bp …, no more`). X still rejects those near-limit roots.
fn peel_incomplete_root_tail(root: &mut String, leftover: &mut String, trailer: &str) {
    if leftover.trim().is_empty() || root_ends_sentence(root) {
        return;
    }
    let Some((keep, frag)) = split_at_last_sentence_end(root) else {
        return;
    };
    if keep.is_empty() || frag.trim().is_empty() {
        return;
    }
    let new_leftover = prepend_paragraph(frag.trim(), leftover.trim());
    if !reply_fits(&new_leftover, trailer) {
        return;
    }
    *root = keep;
    *leftover = new_leftover;
}

fn split_at_last_sentence_end(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let mut last_keep_end: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        let ch = trimmed[i..].chars().next()?;
        let next = i + ch.len_utf8();
        if matches!(ch, '.' | '!' | '?') {
            let mut end = next;
            while end < bytes.len() {
                let Some(c2) = trimmed[end..].chars().next() else {
                    break;
                };
                if c2 == '\n' {
                    break;
                }
                if c2.is_whitespace() || is_emoji_or_symbol_char(c2) {
                    end += c2.len_utf8();
                    continue;
                }
                break;
            }
            last_keep_end = Some(end);
            i = end;
            continue;
        }
        i = next;
    }
    let keep_end = last_keep_end?;
    if keep_end >= trimmed.len() {
        return None;
    }
    let keep = trimmed[..keep_end].trim_end().to_string();
    let frag = trimmed[keep_end..].trim_start().to_string();
    if keep.is_empty() || frag.is_empty() {
        None
    } else {
        Some((keep, frag))
    }
}

fn prepend_paragraph(frag: &str, rest: &str) -> String {
    let frag = frag.trim();
    let rest = rest.trim();
    if frag.is_empty() {
        rest.to_string()
    } else if rest.is_empty() {
        frag.to_string()
    } else {
        format!("{frag}\n\n{rest}")
    }
}

/// First sentence (through `.` / `?` / `!`, plus trailing emoji on the same line) and the rest.
fn take_leading_sentence(text: &str) -> (String, String) {
    let text = text.trim_start();
    if text.is_empty() {
        return (String::new(), String::new());
    }
    let Some((end, _punct)) = find_sentence_end(text) else {
        if let Some(pos) = text.find("\n\n") {
            return (
                text[..pos].trim().to_string(),
                text[pos..].trim_start().to_string(),
            );
        }
        return (text.to_string(), String::new());
    };
    let mut tail = end;
    for (i, ch) in text[end..].char_indices() {
        if ch == '\n' {
            break;
        }
        if ch.is_whitespace() {
            tail = end + i + ch.len_utf8();
            continue;
        }
        if is_emoji_or_symbol_char(ch) {
            tail = end + i + ch.len_utf8();
            continue;
        }
        break;
    }
    let sentence = text[..tail].trim().to_string();
    let rest = text[tail..].trim_start().to_string();
    (sentence, rest)
}

fn find_sentence_end(text: &str) -> Option<(usize, char)> {
    for (i, ch) in text.char_indices() {
        if matches!(ch, '.' | '!' | '?') {
            return Some((i + ch.len_utf8(), ch));
        }
    }
    None
}

fn is_emoji_or_symbol_char(ch: char) -> bool {
    x_cp_weight(ch as u32) > 1 && !ch.is_alphanumeric()
}

fn reply_fits(leftover: &str, trailer: &str) -> bool {
    fits_x_limit(&join_head_trailer(leftover.trim(), trailer))
}

fn take_first_word(text: &str) -> (String, String) {
    let text = text.trim_start();
    let Some(first) = text.split_whitespace().next() else {
        return (String::new(), String::new());
    };
    let rest = text[first.len()..].trim_start().to_string();
    (first.to_string(), rest)
}

fn append_word(root: &str, word: &str) -> String {
    if root.is_empty() {
        word.to_string()
    } else {
        format!("{root} {word}")
    }
}

/// Articles/determiners that must not dangle alone at the end of a root tweet.
fn is_dangling_root_tail_word(word: &str) -> bool {
    let w = word.trim_matches(|c: char| c.is_ascii_punctuation());
    matches!(w.to_ascii_lowercase().as_str(), "the" | "a" | "an" | "one")
}

/// Move a trailing article (`The`, `a`, …) from root onto leftover when reply still fits.
fn rebalance_dangling_root_tail(root: &mut String, leftover: &mut String, trailer: &str) {
    while let Some(last) = root.split_whitespace().last() {
        if !is_dangling_root_tail_word(last) || leftover.trim().is_empty() {
            break;
        }
        let (new_root, Some(word)) = pop_last_word(root) else {
            break;
        };
        let new_leftover = prepend_word(&word, leftover.trim());
        if !reply_fits(&new_leftover, trailer) {
            break;
        }
        *root = new_root;
        *leftover = new_leftover;
    }
}

fn pop_last_word(text: &str) -> (String, Option<String>) {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return (String::new(), None);
    }
    let Some(last_ws) = trimmed.rfind(char::is_whitespace) else {
        return (String::new(), Some(trimmed.to_string()));
    };
    let word = trimmed[last_ws + 1..].trim().to_string();
    let root = trimmed[..last_ws].trim_end().to_string();
    (root, Some(word))
}

fn prepend_word(word: &str, rest: &str) -> String {
    let rest = rest.trim();
    if rest.is_empty() {
        word.to_string()
    } else {
        format!("{word} {rest}")
    }
}

/// Drop a trailing article from `prefix` when `suffix` continues the sentence.
fn trim_dangling_prefix_tail(words: &[&str], mut n: usize) -> usize {
    while n > 0 && n < words.len() && is_dangling_root_tail_word(words[n - 1]) {
        n -= 1;
    }
    n
}

#[must_use]
pub fn strip_thread_marks(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t == THREAD_MARK || is_thread_index(t) {
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
    lines.join("\n")
}

struct Peeled {
    head: String,
    trailer: String,
}

fn peel_trailer(text: &str) -> Peeled {
    let mut lines: Vec<String> = text.lines().map(std::string::ToString::to_string).collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut url = None;
    let mut tag_lines: Vec<String> = Vec::new();
    while let Some(raw) = lines.last() {
        let t = raw.trim().to_string();
        if t.is_empty() {
            lines.pop();
            continue;
        }
        if url.is_none() && is_publisher_cite_line(&t) {
            url = Some(t);
            lines.pop();
            continue;
        }
        if is_hashtag_line(&t) {
            tag_lines.push(t);
            lines.pop();
            continue;
        }
        // Writer often glues tags onto the last prose line. Peel them so overflow
        // still becomes root + reply (tags on tweet 2) instead of one over-long root.
        if let Some((prose, tags)) = peel_trailing_hashtags(&t) {
            lines.pop();
            if !prose.is_empty() {
                lines.push(prose);
            }
            tag_lines.push(tags);
            continue;
        }
        break;
    }
    tag_lines.reverse();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let head = lines.join("\n").trim().to_string();
    let mut trailer_parts = tag_lines;
    if let Some(u) = url {
        trailer_parts.push(u);
    }
    let trailer = join_beats(&trailer_parts);
    Peeled { head, trailer }
}

/// If `line` ends with one or more `#tags` after prose, split them off.
fn peel_trailing_hashtags(line: &str) -> Option<(String, String)> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    let mut tag_start = tokens.len();
    while tag_start > 0 && is_hashtag_token(tokens[tag_start - 1]) {
        tag_start -= 1;
    }
    if tag_start == 0 || tag_start == tokens.len() {
        return None;
    }
    let prose = tokens[..tag_start].join(" ");
    let tags = tokens[tag_start..].join(" ");
    Some((prose, tags))
}

fn is_hashtag_token(tok: &str) -> bool {
    tok.starts_with('#')
        && tok.len() >= 2
        && tok[1..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_publisher_cite_line(t: &str) -> bool {
    // Publisher page or X status: one bare https line; X still renders as a quote card.
    t.starts_with("https://") && is_allowed_tweet_cite(t)
}

fn is_hashtag_line(t: &str) -> bool {
    let mut any = false;
    for tok in t.split_whitespace() {
        if !is_hashtag_token(tok) {
            return false;
        }
        any = true;
    }
    any
}

#[must_use]
pub fn is_thread_chrome_line(line: &str) -> bool {
    let t = line.trim();
    t == THREAD_MARK || is_thread_index(t)
}

fn is_thread_index(t: &str) -> bool {
    let t = t.trim().trim_matches(['(', ')']);
    let mut parts = t.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), None) => {
            !a.is_empty()
                && !b.is_empty()
                && a.chars().all(|c| c.is_ascii_digit())
                && b.chars().all(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

fn join_head_trailer(head: &str, trailer: &str) -> String {
    match (head.is_empty(), trailer.is_empty()) {
        (true, true) => String::new(),
        (false, true) => head.to_string(),
        (true, false) => trailer.to_string(),
        (false, false) => format!("{head}\n\n{trailer}"),
    }
}

fn join_beats(parts: &[String]) -> String {
    parts
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn take_beats_fitting(head: &str, limit: usize) -> (String, String) {
    let beats: Vec<String> = head
        .split("\n\n")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if beats.is_empty() {
        return (String::new(), String::new());
    }
    let mut kept: Vec<String> = Vec::new();
    let mut i = 0;
    while i < beats.len() {
        let mut candidate = kept.clone();
        candidate.push(beats[i].clone());
        let joined = candidate.join("\n\n");
        if fits_limit(&joined, limit) {
            kept.push(beats[i].clone());
            i += 1;
            continue;
        }
        let remain = remaining_after(&kept, limit);
        let (piece, rest_beat) = take_prefix_fitting(&beats[i], remain);
        if !piece.is_empty() {
            kept.push(piece);
            return (kept.join("\n\n"), join_rest(rest_beat, &beats[i + 1..]));
        }
        break;
    }
    if kept.is_empty() {
        let first = trim_to_limit(&beats[0], limit);
        return (first, beats[1..].join("\n\n"));
    }
    (kept.join("\n\n"), beats[i..].join("\n\n"))
}

fn remaining_after(kept: &[String], limit: usize) -> usize {
    if kept.is_empty() {
        return limit;
    }
    let used = x_weighted_len(&kept.join("\n\n"));
    limit.saturating_sub(used).saturating_sub(2)
}

fn remaining_after_lines(kept: &[String], limit: usize) -> usize {
    if kept.is_empty() {
        return limit;
    }
    limit
        .saturating_sub(x_weighted_len(&kept.join("\n")))
        .saturating_sub(1)
}

/// Prefix fits `limit` at word boundaries; suffix is the remaining words/lines.
fn split_word_prefix(text: &str, limit: usize) -> (String, String) {
    let text = text.trim();
    if text.is_empty() {
        return (String::new(), String::new());
    }
    if fits_limit(text, limit) {
        return (text.to_string(), String::new());
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut n = 0usize;
    while n < words.len() {
        let candidate = words[..=n].join(" ");
        if !fits_limit(&candidate, limit) {
            break;
        }
        n += 1;
    }
    n = trim_dangling_prefix_tail(&words, n);
    if n == 0 {
        let prefix = trim_to_limit(text, limit);
        if prefix.is_empty() {
            return (String::new(), text.to_string());
        }
        let suffix = text
            .strip_prefix(prefix.as_str())
            .unwrap_or("")
            .trim_start()
            .to_string();
        return (prefix, suffix);
    }
    let prefix = words[..n].join(" ");
    let suffix = words[n..].join(" ");
    (prefix, suffix)
}

fn join_rest(rest_beat: String, more: &[String]) -> String {
    let mut parts = Vec::new();
    if !rest_beat.trim().is_empty() {
        parts.push(rest_beat);
    }
    parts.extend(more.iter().cloned());
    parts.join("\n\n")
}

fn take_prefix_fitting(text: &str, limit: usize) -> (String, String) {
    if limit == 0 || text.trim().is_empty() {
        return (String::new(), text.trim().to_string());
    }
    if fits_limit(text, limit) {
        return (text.trim().to_string(), String::new());
    }
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if lines.is_empty() {
        return (String::new(), String::new());
    }
    let mut kept: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut candidate = kept.clone();
        candidate.push(lines[i].to_string());
        let joined = candidate.join("\n");
        if fits_limit(&joined, limit) {
            kept.push(lines[i].to_string());
            i += 1;
            continue;
        }
        let remain = remaining_after_lines(&kept, limit);
        let (piece, rest_line) = split_word_prefix(lines[i], remain);
        if !piece.is_empty() {
            kept.push(piece);
            let mut tail: Vec<String> = Vec::new();
            if !rest_line.is_empty() {
                tail.push(rest_line);
            }
            tail.extend(lines[i + 1..].iter().map(|s| (*s).to_string()));
            return (kept.join("\n"), tail.join("\n"));
        }
        break;
    }
    if kept.is_empty() {
        return split_word_prefix(text, limit);
    }
    (kept.join("\n"), lines[i..].join("\n"))
}

fn pack_tweet_two(leftover: &str, trailer: &str) -> String {
    join_head_trailer(leftover.trim(), trailer)
}

fn fits_limit(text: &str, limit: usize) -> bool {
    x_weighted_len(text) <= limit
}

/// Prefer dropping whole blank-separated beats from the end over mid-sentence char trim.
fn fit_by_dropping_beats(head: &str) -> String {
    let head = head.trim();
    if head.is_empty() {
        return String::new();
    }
    if fits_x_limit(head) {
        return head.to_string();
    }
    let mut beats: Vec<String> = head
        .split("\n\n")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    while beats.len() > 1 {
        beats.pop();
        let joined = beats.join("\n\n");
        if fits_x_limit(&joined) {
            return joined;
        }
    }
    trim_to_limit(&beats[0], X_CHAR_LIMIT)
}

fn trim_to_limit(text: &str, limit: usize) -> String {
    if fits_limit(text, limit) {
        return text.trim().to_string();
    }
    let mut out = String::new();
    for word in text.split_whitespace() {
        let candidate = if out.is_empty() {
            word.to_string()
        } else {
            format!("{out} {word}")
        };
        if !fits_limit(&candidate, limit) {
            break;
        }
        out = candidate;
    }
    if out.is_empty() {
        let mut buf = String::new();
        for ch in text.chars() {
            let mut next = buf.clone();
            next.push(ch);
            if !fits_limit(&next, limit) {
                break;
            }
            buf = next;
        }
        buf.trim().to_string()
    } else {
        out
    }
}

fn url_end_at(text: &str, i: usize) -> Option<usize> {
    let rest = &text[i..];
    if let Some(rel) = scheme_url_len(rest) {
        return Some(i + rel);
    }
    if !url_start_boundary(text, i) {
        return None;
    }
    bare_autolink_len(rest).map(|rel| i + rel)
}

fn url_start_boundary(text: &str, i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let Some(prev) = text[..i].chars().next_back() else {
        return true;
    };
    // X does not start an autolink mid-token or after `@` / `/`.
    !prev.is_ascii_alphanumeric() && !matches!(prev, '@' | '/' | '_' | '-' | '.')
}

fn scheme_url_len(rest: &str) -> Option<usize> {
    let scheme = if rest.starts_with("https://") {
        8
    } else if rest.starts_with("http://") {
        7
    } else {
        return None;
    };
    let mut end = scheme;
    for ch in rest[scheme..].chars() {
        if ch.is_whitespace() {
            break;
        }
        end += ch.len_utf8();
    }
    (end > scheme).then_some(trim_url_trailing_punct(rest, end))
}

/// Bare `crates.io` / `example.com/path` — X weights these as t.co (23), not raw chars.
fn bare_autolink_len(rest: &str) -> Option<usize> {
    let token = rest.split_whitespace().next()?;
    let token = trim_url_token_punct(token);
    if token.is_empty() {
        return None;
    }
    let host = token.split_once('/').map_or(token, |(h, _)| h);
    if !is_x_autolink_host(host) {
        return None;
    }
    Some(trim_url_trailing_punct(rest, token.len()))
}

fn is_x_autolink_host(host: &str) -> bool {
    let host = host.trim_matches('.');
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let Some(tld) = labels.last() else {
        return false;
    };
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    labels[..labels.len() - 1].iter().all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

fn trim_url_token_punct(token: &str) -> &str {
    token.trim_end_matches(|c: char| {
        matches!(
            c,
            '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '"' | '\''
        )
    })
}

fn trim_url_trailing_punct(rest: &str, mut end: usize) -> usize {
    while end > 0 {
        let Some(ch) = rest[..end].chars().next_back() else {
            break;
        };
        if matches!(
            ch,
            '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '"' | '\''
        ) {
            end -= ch.len_utf8();
            continue;
        }
        break;
    }
    end
}

fn x_cp_weight(cp: u32) -> usize {
    const LIGHT: &[(u32, u32)] = &[(0, 4351), (8192, 8205), (8208, 8223), (8242, 8247)];
    if LIGHT.iter().any(|&(a, b)| cp >= a && cp <= b) {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shipped wrong on XPOST-20260822-000062: hook + two insight beats + tag trailer.
    const AGENTPAY_HOOK_INSIGHT: &str = "\
📜 CSPR AgentPay Guard is the firewall before an AI agent pays, HTTP 402 rules, allowlists, and replay protection all in one. 🚀

It's not just about spending limits, it's about securing the whole flow: budget, expiry, audit trails, and even mock local tests with real Casper Testnet proof. 🐚

MVP, not production custody. But if you're building on Casper, this is your guardrail. 🔐

#CSPR #AgentPay #OnChain #AI";

    const AGENTPAY_PUBLISHER_URL: &str = "https://alsaecas.dev/projects/cspr-agentpay-guard";

    fn assert_agentpay_forward_word_split(parts: &[String], expect_url_on_reply: bool) {
        assert_eq!(parts.len(), 2, "{parts:?}");
        let root = &parts[0];
        let reply = &parts[1];
        assert!(fits_x_limit(root), "root {} chars", x_weighted_len(root));
        assert!(fits_x_limit(reply), "reply {} chars", x_weighted_len(reply));
        assert!(
            root.starts_with('📜') && root.contains("AgentPay Guard"),
            "root starts from the top: {root}"
        );
        assert!(
            root.contains("spending limits"),
            "root fills forward through word boundary: {root}"
        );
        assert!(!root.contains('#'), "root has no tags: {root}");
        assert!(
            !reply.trim_end().ends_with("building on"),
            "reply must not dangling mid-sentence trim: {reply}"
        );
        assert!(
            reply.contains("Testnet proof")
                || reply.contains("it's about")
                || reply.contains("securing the whole flow"),
            "reply continues from root word split: {reply}"
        );
        assert!(
            reply.contains("guardrail") && reply.contains("#CSPR"),
            "reply has rest + tags: {reply}"
        );
        assert!(
            !root.trim_end().ends_with("building on"),
            "root must not mid-cut dangling: {root}"
        );
        if expect_url_on_reply {
            assert!(
                reply.contains("alsaecas.dev"),
                "reply carries publisher link: {reply}"
            );
        }
    }

    const SAMPLE: &str = "\
🦉 GitHub Models' retirement feels like a quiet end to a promising experiment.
Sad to see such a tool fade-especially when free alternatives are scarce.

Microsoft Foundry and Copilot?
Not exactly the open-source dream we hoped for.

Builders, keep an eye on migration paths.
The future of AI tools is still in flux.

#AI #GitHub #ModelRetirement

https://blog.dante.company/en/articles/github-models-retirement-migration-2026-07-02";

    #[test]
    fn inline_trailing_hashtags_peel_for_split() {
        // Same shape as TWEET-20260820-000046 after cite URL strip: tags glued on prose.
        let text = "\
📜 @github’s 2026 outage crisis is real, 257 incidents, 48 major outages, and a 50% repo download error rate. 🚀 The root? Autoscaling fails + VS Code retry storms. 🦀 But they’re not just fixing it, they’re shipping fixes and new features like stacked PRs. #CloudOps #DevTools #GitHub #OutageFixes";
        assert!(
            !fits_x_limit(text),
            "fixture must overflow before peel: {}",
            x_weighted_len(text)
        );
        let parts = layout_x_thread(text);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(fits_x_limit(&parts[0]), "{}", x_weighted_len(&parts[0]));
        assert!(fits_x_limit(&parts[1]), "{}", x_weighted_len(&parts[1]));
        assert!(!parts[0].contains('#'), "root: {}", parts[0]);
        assert!(parts[1].contains("#CloudOps"), "reply: {}", parts[1]);
    }

    #[test]
    fn xpost073_battery_packs_bare_domain_and_no_midcut() {
        // Shipped fail XPOST-20260825-000073: our counter treated crates.io as 9 chars;
        // X autolinks it as t.co (23). Root was filled to ~276 then Post stayed disabled.
        let body = "\
📜 Rust devs, meet your new CLI toolkit, a curated crate pack that’s like a 🦀-powered toolbox for your workflow. No more sifting through crates.io chaos. Just swap in the right batteries for your project. 🚀

Cargo bp lets you add these packs with `cargo bp add cli`, no more dependency guesswork. It’s about making your stack feel like a 🐦-fueled ride. 🧠

#Rust #CLI #Cargo #Crates

https://smallcultfollowing.com/babysteps/blog/2026/07/15/battery-packs";
        assert_eq!(
            x_weighted_len("crates.io"),
            TCO_LEN,
            "bare domain must count as t.co"
        );
        let parts = layout_x_thread(body);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(
            fits_x_limit(&parts[0]),
            "root {}",
            x_weighted_len(&parts[0])
        );
        assert!(
            fits_x_limit(&parts[1]),
            "reply {}",
            x_weighted_len(&parts[1])
        );
        assert!(
            !parts[0].trim_end().ends_with("no more"),
            "root must not mid-cut: {}",
            parts[0]
        );
        assert!(
            root_ends_sentence(&parts[0]),
            "root should end on a sentence: {}",
            parts[0]
        );
        assert!(parts[1].contains("Cargo bp") || parts[1].contains("dependency guesswork"));
        assert!(parts[1].contains("#Rust"));
        assert!(parts[1].contains("smallcultfollowing.com"));
    }

    #[test]
    fn sample_splits_commentary_then_tags_and_url() {
        let parts = layout_x_thread(SAMPLE);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(fits_x_limit(&parts[0]));
        assert!(fits_x_limit(&parts[1]));
        assert!(parts[0].contains("GitHub Models"));
        assert!(
            parts[0].starts_with('🦉') || parts[0].contains("GitHub Models"),
            "root starts at the beginning, not the end: {}",
            parts[0]
        );
        assert!(!parts[0].contains("blog.dante.company"));
        assert!(!parts[0].contains("#AI"));
        assert!(parts[1].contains("#AI #GitHub #ModelRetirement"));
        assert!(parts[1].contains("https://blog.dante.company/"));
        assert!(
            !parts[0]
                .split_whitespace()
                .last()
                .is_some_and(is_dangling_root_tail_word),
            "root must not end on a dangling article: {}",
            parts[0]
        );
        assert!(
            parts[1].starts_with("The future"),
            "reply carries the peeled article: {}",
            parts[1]
        );
        assert!(x_weighted_len(&parts[0]) > 200);
        assert!(
            parts[0].contains("future of AI tools is still in flux")
                || parts[1].contains("future of AI tools is still in flux")
        );
    }

    #[test]
    fn root_never_ends_on_dangling_article() {
        let cases = [
            SAMPLE,
            AGENTPAY_HOOK_INSIGHT,
            "Builders keep an eye on the path.\n\nThe next wave is here.\n\n#AI\n\nhttps://example.com/x",
        ];
        for text in cases {
            for part in layout_x_thread(text) {
                if let Some(last) = part.split_whitespace().last() {
                    assert!(
                        !is_dangling_root_tail_word(last),
                        "dangling tail `{last}` in: {part}"
                    );
                }
            }
        }
    }

    #[test]
    fn trim_dangling_prefix_tail_peels_the() {
        let words: Vec<&str> = "paths. The future unfolds".split_whitespace().collect();
        assert_eq!(trim_dangling_prefix_tail(&words, 2), 1);
    }

    #[test]
    fn grow_stops_at_sentence_boundary_not_bare_words() {
        let head = "\
🦉 GitHub Models' retirement feels like a quiet end to a promising experiment.
Sad to see such a tool fade-especially when free alternatives are scarce.

Microsoft Foundry and Copilot?
Not exactly the open-source dream we hoped for.

Builders, keep an eye on migration paths.
The future of AI tools is still in flux.";
        let trailer = "#AI #GitHub #ModelRetirement\n\nhttps://blog.dante.company/en/articles/github-models-retirement-migration-2026-07-02";
        let parts = forward_fill_split_only(head, trailer);
        assert_eq!(parts.len(), 2);
        assert!(
            parts[0].trim_end().ends_with("migration paths."),
            "root ends on sentence boundary: {}",
            parts[0]
        );
        assert!(
            parts[1].starts_with("The future"),
            "reply opens the next sentence: {}",
            parts[1]
        );
    }

    #[test]
    fn take_leading_sentence_includes_trailing_emoji() {
        let (sent, rest) = take_leading_sentence("Testnet proof. 🐚\n\nMVP next.");
        assert_eq!(sent, "Testnet proof. 🐚");
        assert_eq!(rest, "MVP next.");
    }

    fn forward_fill_split_only(head: &str, trailer: &str) -> Vec<String> {
        let (mut root, mut leftover) = take_beats_fitting(head, X_CHAR_LIMIT);
        grow_root_until_reply_fits(&mut root, &mut leftover, trailer);
        rebalance_dangling_root_tail(&mut root, &mut leftover, trailer);
        peel_incomplete_root_tail(&mut root, &mut leftover, trailer);
        let reply = pack_tweet_two(&leftover, trailer);
        vec![root, reply]
    }

    #[test]
    fn short_tweet_stays_one() {
        let parts = layout_x_thread("Hello builders.\n\nhttps://labs.sogeti.com/a");
        assert_eq!(parts.len(), 1);
        assert!(parts[0].contains("Hello builders"));
        assert!(parts[0].contains("https://labs.sogeti.com/a"));
    }

    #[test]
    fn strip_ignores_thread_chrome() {
        let raw = "1/2\nHello builders\n\n--- thread ---\n\n2/2\n#AI\n\nhttps://labs.sogeti.com/a";
        let parts = layout_x_thread(raw);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].contains("Hello builders"));
        assert!(parts[0].contains("#AI"));
    }

    #[test]
    fn layout_is_stable() {
        let parts = layout_x_thread(SAMPLE);
        let again = layout_x_thread(SAMPLE);
        assert_eq!(again, parts);
        assert!(fits_x_limit(&parts[0]));
        assert!(fits_x_limit(&parts[1]));
    }

    #[test]
    fn quote_overflow_puts_tags_on_tweet_two() {
        let long = format!(
            "{}\n\n#AI #GitHub",
            "Builders keep an eye on migration paths. ".repeat(12)
        );
        let parts = layout_x_thread(&long);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(parts[0].starts_with("Builders keep an eye"));
        assert!(parts.last().unwrap().contains("#AI"));
        assert!(fits_x_limit(&parts[0]));
        assert!(fits_x_limit(&parts[1]));
    }

    #[test]
    fn prose_only_overflow_stays_one_root_no_self_reply() {
        let long = format!(
            "⚡ tinyboot fits in 1920 bytes.\n\n{}\n\nThat’s smart engineering for builders who care about flash.",
            "It leaves every byte of user flash free, which means more room for your app. "
                .repeat(3)
        );
        let parts = layout_x_thread(&long);
        assert_eq!(
            parts.len(),
            1,
            "no tags/URL → do not invent a self-reply: {parts:?}"
        );
        assert!(parts[0].contains("tinyboot"));
        assert!(fits_x_limit(&parts[0]));
        assert!(!parts[0].contains("https://x.com/"));
        assert!(
            !parts[0].trim_end().ends_with("easier"),
            "must not mid-sentence trim: {}",
            parts[0]
        );
    }

    #[test]
    fn x_status_url_stays_as_link_like_publisher() {
        let body = "\
@Interchouette
⚡ tinyboot is a clever Rust bootloader that fits in just 1920 bytes-perfect for microcontrollers with tight memory.

It leaves every byte of user flash free, which means more room for your app.

That’s the kind of smart engineering that makes embedded systems easier to build.

#Rust #Embedded #OpenSource

https://x.com/AstraKernel/status/2088224406187413962";
        let text = crate::sources::tweet_footer::strip_own_x_handle(body);
        let parts = layout_x_thread(&text);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(parts[0].contains("tinyboot"));
        assert!(!parts.join("\n").contains("@Interchouette"));
        assert!(
            parts[1].contains("x.com/AstraKernel"),
            "X URL on reply: {}",
            parts[1]
        );
        assert!(parts[1].contains("#Rust"), "tags on reply: {}", parts[1]);
        assert!(
            !parts[0].trim().starts_with("That’s the kind"),
            "must not start from the end: {}",
            parts[0]
        );
    }

    #[test]
    fn agentpay_reply_is_full_remainder_not_word_trimmed() {
        let text = format!("{AGENTPAY_HOOK_INSIGHT}\n\n{AGENTPAY_PUBLISHER_URL}");
        let parts = layout_x_thread(&text);
        assert_eq!(parts.len(), 2);
        assert!(
            !parts[1].trim_end().ends_with("building on"),
            "reply must not be word-trimmed mid-sentence: {}",
            parts[1]
        );
        assert!(
            parts[1].contains("guardrail") && parts[1].contains("#CSPR"),
            "reply keeps full tail: {}",
            parts[1]
        );
        assert!(
            x_weighted_len(&parts[0]) > 200,
            "root near 280, not hook-only: {}",
            x_weighted_len(&parts[0])
        );
    }

    #[test]
    fn agentpay_forward_word_split_with_publisher_url() {
        let text = format!("{AGENTPAY_HOOK_INSIGHT}\n\n{AGENTPAY_PUBLISHER_URL}");
        let parts = layout_x_thread(&text);
        assert_agentpay_forward_word_split(&parts, true);
    }

    #[test]
    fn agentpay_forward_word_split_tags_only() {
        let parts = layout_x_thread(AGENTPAY_HOOK_INSIGHT);
        assert_agentpay_forward_word_split(&parts, false);
    }

    #[test]
    fn split_word_prefix_respects_weighted_limit() {
        let (prefix, suffix) = split_word_prefix(
            "It's not just about spending limits, it's about securing the whole flow.",
            40,
        );
        assert!(fits_x_limit(&prefix));
        assert!(!suffix.is_empty());
        assert!(prefix.ends_with("limits,") || prefix.contains("spending"));
    }

    #[test]
    fn agentpay_forward_split_puts_hook_on_root() {
        let text = format!("{AGENTPAY_HOOK_INSIGHT}\n\n{AGENTPAY_PUBLISHER_URL}");
        assert_agentpay_forward_word_split(&layout_x_thread(&text), true);
    }

    #[test]
    #[ignore = "manual: cargo test -p itcy split_showcase_dump -- --ignored --nocapture"]
    fn split_showcase_dump() {
        fn dump(name: &str, text: &str) {
            let parts = layout_x_thread(text);
            eprintln!("\n========== {name} ==========");
            eprintln!("INPUT weighted {} / 280", x_weighted_len(text));
            eprintln!("SPLIT: {} tweet(s)", parts.len());
            for (i, p) in parts.iter().enumerate() {
                let label = if i == 0 { "ROOT" } else { "REPLY" };
                eprintln!("--- {label} (weighted {} / 280) ---", x_weighted_len(p));
                eprintln!("{p}");
            }
        }

        dump(
            "AgentPay XPOST-000062 (layout only, tags+URL)",
            &format!("{AGENTPAY_HOOK_INSIGHT}\n\n{AGENTPAY_PUBLISHER_URL}"),
        );
        dump(
            "AgentPay tags only (no publisher URL)",
            AGENTPAY_HOOK_INSIGHT,
        );
        dump("GitHub Models SAMPLE", SAMPLE);
        dump(
            "GitHub outage TWEET-000046 (inline trailing tags)",
            "📜 @github's 2026 outage crisis is real, 257 incidents, 48 major outages, and a 50% repo download error rate. 🚀 The root? Autoscaling fails + VS Code retry storms. 🦀 But they're not just fixing it, they're shipping fixes and new features like stacked PRs. #CloudOps #DevTools #GitHub #OutageFixes",
        );
        let tinyboot = "\
⚡ tinyboot is a clever Rust bootloader that fits in just 1920 bytes-perfect for microcontrollers with tight memory.

It leaves every byte of user flash free, which means more room for your app.

That's the kind of smart engineering that makes embedded systems easier to build.

#Rust #Embedded #OpenSource

https://x.com/AstraKernel/status/2088224406187413962";
        dump("tinyboot + X status URL", tinyboot);
        dump(
            "GPUI TWEET-000047 (fits one tweet)",
            "📜 Rust GUI just got a GPU-powered upgrade.\n🦀 GPUI brings 60+ solid components, huge-data tables, and a smooth 200K-line code editor, no more wrestling with Qt.\n🦉 Native feel, dock layouts, themes… all in one.\n\n#Rust #GUI #OpenSource\n\nhttps://x.com/milonspace/status/2089661151529574481",
        );
        dump(
            "Short (stays one)",
            "Hello builders.\n\nhttps://labs.sogeti.com/a",
        );

        // Full ship path (Slack compose footer stripped by tweet_text_for_api).
        let agentpay_ship = "\
Tweet ID: TWEET-20260822-000062

📜 CSPR AgentPay Guard is the firewall before an AI agent pays, HTTP 402 rules, allowlists, and replay protection all in one. 🚀

It's not just about spending limits, it's about securing the whole flow: budget, expiry, audit trails, and even mock local tests with real Casper Testnet proof. 🐚

MVP, not production custody. But if you're building on Casper, this is your guardrail. 🔐

#CSPR #AgentPay #OnChain #AI

https://alsaecas.dev/projects/cspr-agentpay-guard

Link: 1
0 = no link. /change_url TWEET-20260822-000062 <0|1|2|3|url>
1. https://alsaecas.dev/projects/cspr-agentpay-guard

Written by AI - ITCy - model ollama/qwen3:8b - tokens in:6146 out:123";
        let api_text = crate::publish::tweet_text_for_api(agentpay_ship);
        eprintln!("\n========== AgentPay full ship body (tweet_text_for_api) ==========");
        eprintln!("CLEANED:\n{api_text}\n");
        dump("AgentPay ship split (via layout_x_thread)", &api_text);
        let ship_parts = crate::publish::tweet_texts_for_api(agentpay_ship);
        eprintln!(
            "tweet_texts_for_api: {} part(s), matches layout: {}",
            ship_parts.len(),
            ship_parts == layout_x_thread(&api_text)
        );
    }

    #[test]
    #[ignore = "manual: cargo test -p itcy agentpay_split_debug_dump -- --ignored --nocapture"]
    fn agentpay_split_debug_dump() {
        let raw = "\
:scroll: CSPR AgentPay Guard is the firewall before an AI agent pays, HTTP 402 rules, allowlists, and replay protection all in one. :rocket:

It's not just about spending limits, it's about securing the whole flow: budget, expiry, audit trails, and even mock local tests with real Casper Testnet proof. :shell:

MVP, not production custody. But if you're building on Casper, this is your guardrail. :closed_lock_with_key:

#CSPR #AgentPay #OnChain #AI

https://alsaecas.dev/projects/cspr-agentpay-guard";
        let body = format!("Tweet ID: TWEET-DEBUG\n\n{raw}");
        let cleaned = crate::publish::tweet_text_for_api(&body);
        let parts = layout_x_thread(&cleaned);
        eprintln!("=== CLEANED (emoji expanded) ===\n{cleaned}\n");
        eprintln!("=== SPLIT: {} tweet(s) ===", parts.len());
        for (i, p) in parts.iter().enumerate() {
            let label = if i == 0 {
                "ROOT (posted first)"
            } else {
                "REPLY"
            };
            eprintln!(
                "--- {label} (weighted {} / 280) ---\n{p}\n",
                x_weighted_len(p)
            );
        }
        let texts = crate::publish::tweet_texts_for_api(&body);
        eprintln!("=== tweet_texts_for_api matches layout: {}", texts == parts);
    }

    #[test]
    fn url_counts_as_tco() {
        let u =
            "https://blog.dante.company/en/articles/github-models-retirement-migration-2026-07-02";
        assert_eq!(x_weighted_len(u), TCO_LEN);
        assert_eq!(
            x_weighted_len("hi https://example.com/x zz"),
            2 + 1 + TCO_LEN + 1 + 2
        );
        assert_eq!(x_weighted_len("crates.io"), TCO_LEN);
        assert_eq!(
            x_weighted_len("through crates.io chaos"),
            "through ".len() + TCO_LEN + " chaos".len()
        );
        assert!(
            x_weighted_len("v1.2.3") < TCO_LEN,
            "version numbers must not count as t.co"
        );
    }
}

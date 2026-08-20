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

/// Commentary on tweet 1; hashtags + publisher URL on tweet 2 when 280 is tight.
///
/// Order is locked: root first (start of the copy), then reply (trailer / leftover).
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
    let (tweet1, leftover) = take_beats_fitting(&peeled.head, X_CHAR_LIMIT);
    let tweet2 = pack_tweet_two(&leftover, &peeled.trailer);
    let mut out = Vec::new();
    if !tweet1.is_empty() {
        out.push(tweet1);
    }
    if !tweet2.is_empty() {
        out.push(tweet2);
    }
    if out.is_empty() {
        vec![trim_to_limit(&text, X_CHAR_LIMIT)]
    } else {
        out
    }
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
    let mut kept: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut candidate = kept.clone();
        candidate.push(lines[i]);
        let joined = candidate.join("\n");
        if fits_limit(&joined, limit) {
            kept.push(lines[i]);
            i += 1;
            continue;
        }
        break;
    }
    if kept.is_empty() {
        return (String::new(), text.trim().to_string());
    }
    (kept.join("\n"), lines[i..].join("\n"))
}

fn pack_tweet_two(leftover: &str, trailer: &str) -> String {
    let leftover = leftover.trim();
    let trailer = trailer.trim();
    if leftover.is_empty() {
        return trailer.to_string();
    }
    if trailer.is_empty() {
        return trim_to_limit(leftover, X_CHAR_LIMIT);
    }
    let both = format!("{leftover}\n\n{trailer}");
    if fits_x_limit(&both) {
        return both;
    }
    let trailer_len = x_weighted_len(trailer);
    if trailer_len >= X_CHAR_LIMIT {
        return trim_to_limit(trailer, X_CHAR_LIMIT);
    }
    let budget = X_CHAR_LIMIT.saturating_sub(trailer_len.saturating_add(2));
    let head = trim_to_limit(leftover, budget);
    if head.is_empty() {
        trailer.to_string()
    } else {
        format!("{head}\n\n{trailer}")
    }
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
    let scheme = if rest.starts_with("https://") {
        8
    } else if rest.starts_with("http://") {
        7
    } else {
        return None;
    };
    let mut end = i + scheme;
    for ch in rest[scheme..].chars() {
        if ch.is_whitespace() {
            break;
        }
        end += ch.len_utf8();
    }
    (end > i + scheme).then_some(end)
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
        assert!(x_weighted_len(&parts[0]) > 200);
        assert!(
            parts[0].contains("future of AI tools is still in flux")
                || parts[1].contains("future of AI tools is still in flux")
        );
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
    fn url_counts_as_tco() {
        let u =
            "https://blog.dante.company/en/articles/github-models-retirement-migration-2026-07-02";
        assert_eq!(x_weighted_len(u), TCO_LEN);
        assert_eq!(
            x_weighted_len("hi https://example.com/x zz"),
            2 + 1 + TCO_LEN + 1 + 2
        );
    }
}

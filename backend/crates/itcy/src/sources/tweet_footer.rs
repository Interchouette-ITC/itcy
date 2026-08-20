// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Tweet IDs (`TWEET-YYYYMMDD-NNNNNN`) and Slack cite footer.

use crate::sources::url_hygiene::{
    extract_https_urls, filter_tweet_cite_urls, is_allowed_tweet_cite, is_junk_or_search_url,
    is_x_status_url, url_in_allowlist, x_status_id, X_PUBLIC_HANDLE,
};
use crate::sqlite::open_configured;
use chrono::Local;
use rusqlite::params;
use std::fmt::Write;
use std::path::Path;
use thiserror::Error;

const CODE_SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS tweet_code_seq (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    next_ord INTEGER NOT NULL
);
INSERT OR IGNORE INTO tweet_code_seq (id, next_ord) VALUES (1, 0);
";

#[derive(Debug, Error)]
pub enum TweetFooterError {
    #[error("tweet code store: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("tweet code: {0}")]
    Other(String),
}

/// Allocate next tweet id: `TWEET-YYYYMMDD-NNNNNN` (monotonic; never wraps).
///
/// # Errors
///
/// Returns [`TweetFooterError::Db`] on `SQLite` failure, or [`TweetFooterError::Other`] for path/IO failures.
pub fn next_tweet_id(db_path: &Path) -> Result<String, TweetFooterError> {
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| TweetFooterError::Other(format!("create parent: {e}")))?;
        }
    }
    let conn = open_configured(db_path)?;
    conn.execute_batch(CODE_SCHEMA)?;
    let ord: i64 = conn.query_row(
        "SELECT next_ord FROM tweet_code_seq WHERE id = 1",
        [],
        |r| r.get(0),
    )?;
    let next = ord.saturating_add(1);
    conn.execute(
        "UPDATE tweet_code_seq SET next_ord = ?1 WHERE id = 1",
        params![next],
    )?;
    let day = Local::now().format("%Y%m%d");
    Ok(format!("TWEET-{day}-{next:06}"))
}

/// Up to 3 pack cites (publisher **and** X status). Operator picks `1`/`2`/`3`.
#[must_use]
pub fn pick_tweet_cite_options(pack_urls: &[String], body: &str) -> Vec<String> {
    use crate::sources::url_hygiene::{same_publisher_url, scrub_https_url};

    let pack = filter_tweet_cite_urls(pack_urls);
    let body_urls: Vec<String> = extract_https_urls(body)
        .into_iter()
        .filter(|u| {
            !is_junk_or_search_url(u) && is_allowed_tweet_cite(u) && url_in_allowlist(u, &pack)
        })
        .collect();
    let mut out: Vec<String> = Vec::new();
    for u in body_urls.iter().chain(pack.iter()) {
        let scrubbed = scrub_https_url(u);
        if !is_allowed_tweet_cite(&scrubbed) {
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

/// First https for the in-tweet link (publisher **or** X status). Same rule for both.
#[must_use]
pub fn in_tweet_publisher_url(cites: &[String]) -> Option<&str> {
    cites
        .iter()
        .map(String::as_str)
        .find(|u| is_allowed_tweet_cite(u))
}

/// Set or clear the single in-tweet https line (publisher or X status).
#[must_use]
pub fn ensure_tweet_cite_line(body: &str, in_tweet_url: Option<&str>) -> String {
    // Drop stray status lines first; `set_single_in_post_url` writes the chosen URL.
    let body = strip_bare_x_status_lines(body);
    crate::sources::draft_url::set_single_in_post_url(&body, in_tweet_url.unwrap_or(""))
}

/// Ensure `url` is in `options` (max 3) without dropping other entries when possible.
pub fn ensure_option(options: &mut Vec<String>, url: &str) {
    let url = url.trim();
    if url.is_empty() || options.iter().any(|u| same_https(u, url)) {
        return;
    }
    if options.len() < 3 {
        options.push(url.to_string());
    } else {
        crate::sources::draft_url::promote_link_option(options, url);
    }
}

fn same_https(a: &str, b: &str) -> bool {
    crate::sources::url_hygiene::same_publisher_url(a, b)
}

fn link_index(options: &[String], url: &str) -> Option<usize> {
    options
        .iter()
        .position(|u| same_https(u, url))
        .map(|i| i + 1)
}

/// Tweet ID, body, `Link: N` / `0`, then options `1`/`2`/`3`.
#[must_use]
pub fn compose_tweet_message(body: &str, tweet_id: &str, cites: &[String]) -> String {
    let body = crate::sources::draft_url::strip_sources_section(body);
    let in_tweet = crate::sources::draft_url::extract_in_post_url(&body);
    let mut out = format!("Tweet ID: {tweet_id}\n\n");
    out.push_str(body.trim());
    out.push_str("\n\n");
    match (
        &in_tweet,
        in_tweet.as_deref().and_then(|u| link_index(cites, u)),
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
    let _ = writeln!(out, "0 = no link. /change_url {tweet_id} <0|1|2|3|url>");
    for (i, u) in cites.iter().take(3).enumerate() {
        let _ = writeln!(out, "{}. {u}", i + 1);
    }
    out.trim_end().to_string()
}

/// Simplified tweet message for self-introduction commands: Tweet ID + body only, no link picker.
///
/// The URL is already baked into the tweet body by the self-intro writer.
#[must_use]
pub fn compose_self_intro_tweet_message(body: &str, tweet_id: &str) -> String {
    let body = crate::sources::draft_url::strip_sources_section(body);
    format!("Tweet ID: {tweet_id}\n\n{}", body.trim())
}

/// Apply `/change_url` on a tweet: chosen URL becomes option **1** and the https line.
///
/// `0` clears the https line (options list unchanged). No special-case lecture.
///
/// # Errors
///
/// Returns an operator-facing message when the choice is invalid.
pub fn apply_change_tweet_url(
    tweet_id: &str,
    body: &str,
    link_options: &[String],
    choice: &str,
) -> Result<(String, Vec<String>), String> {
    let mut options = link_options.to_vec();
    let picked = crate::sources::draft_url::resolve_url_choice(choice, &options)?;
    let mut head = body.to_string();
    if let Some(i) = crate::sources::draft_url::footer_start(&head) {
        head = head[..i].to_string();
    }
    if head.starts_with("Tweet ID:") {
        if let Some((_, rest)) = head.split_once('\n') {
            head = rest.trim_start().to_string();
        }
    }
    let head = head.trim();
    match picked {
        crate::sources::draft_url::UrlChoice::Clear => {
            let body = ensure_tweet_cite_line(head, None);
            Ok((compose_tweet_message(&body, tweet_id, &options), options))
        }
        crate::sources::draft_url::UrlChoice::Url(new_url) => {
            if !is_allowed_tweet_cite(&new_url) {
                return Err("use 0, 1, 2, 3, or an https URL".into());
            }
            crate::sources::draft_url::promote_link_option(&mut options, &new_url);
            // Operator pick is the in-tweet link (publisher or X status). One https line.
            let body = ensure_tweet_cite_line(head, Some(new_url.as_str()));
            Ok((compose_tweet_message(&body, tweet_id, &options), options))
        }
    }
}

/// X status id for quote ship (any X status in `cites`).
#[must_use]
pub fn quote_tweet_id_from_cites(cites: &[String]) -> Option<String> {
    cites.iter().find_map(|u| x_status_id(u))
}

/// First publisher in the list, else first entry.
#[must_use]
pub fn primary_cite(cites: &[String]) -> Option<&str> {
    in_tweet_publisher_url(cites).or_else(|| cites.first().map(String::as_str))
}

/// Drop own-handle spam (`@Interchouette`). The account posts as itself; never @-mentions itself.
#[must_use]
pub fn strip_own_x_handle(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if is_own_handle_mention(t) {
            continue;
        }
        let cleaned = strip_leading_own_handle(t);
        if cleaned.is_empty() {
            if t.is_empty() {
                lines.push(String::new());
            }
            continue;
        }
        lines.push(cleaned);
    }
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn is_own_handle_mention(token: &str) -> bool {
    token
        .strip_prefix('@')
        .is_some_and(|h| h.eq_ignore_ascii_case(X_PUBLIC_HANDLE))
}

fn strip_leading_own_handle(line: &str) -> String {
    let Some(after_at) = line.strip_prefix('@') else {
        return line.to_string();
    };
    if after_at.len() >= X_PUBLIC_HANDLE.len() {
        let (handle, rest) = after_at.split_at(X_PUBLIC_HANDLE.len());
        if handle.eq_ignore_ascii_case(X_PUBLIC_HANDLE) {
            return rest.trim_start().to_string();
        }
    }
    line.to_string()
}

/// Strip bare X/Twitter status URL lines from tweet text (quote ship carries the cite).
#[must_use]
pub fn strip_bare_x_status_lines(body: &str) -> String {
    let split_at = crate::sources::draft_url::footer_start(body).unwrap_or(body.len());
    let (head, tail) = body.split_at(split_at);
    let mut lines: Vec<&str> = Vec::new();
    for line in head.lines() {
        if is_x_status_url(line.trim()) {
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut out = lines.join("\n");
    if !tail.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(tail);
    }
    out
}

/// First allowed https in the operator brief (instructions, else anywhere in the text).
///
/// A URL in `/tweet_about` / `/draft_about` instructions **is** the cite. No keyword.
#[must_use]
pub fn extract_brief_cite(brief: &str) -> Option<String> {
    extract_https_urls(brief)
        .into_iter()
        .find(|u| is_allowed_tweet_cite(u))
}

/// Keywords from the operator subject (first comma clause; drop URLs and filler).
#[must_use]
pub fn search_keywords(brief: &str) -> Vec<String> {
    let mut s = brief.to_string();
    for u in extract_https_urls(brief) {
        s = s.replace(&u, " ");
    }
    let clause = s.split(',').next().unwrap_or(&s);
    let mut out: Vec<String> = Vec::new();
    for raw in clause.split(|c: char| !(c.is_ascii_alphanumeric() || c == '#' || c == '$')) {
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        let low = t.to_ascii_lowercase();
        if SEARCH_STOP.contains(&low.as_str()) {
            continue;
        }
        if t.len() < 2 && !t.bytes().any(|b| b.is_ascii_digit()) {
            continue;
        }
        if out.iter().any(|x| x.eq_ignore_ascii_case(t)) {
            continue;
        }
        out.push(t.to_string());
        if out.len() == 6 {
            break;
        }
    }
    out
}

const SEARCH_STOP: &[&str] = &[
    "a", "an", "the", "and", "or", "vs", "versus", "of", "for", "to", "in", "on", "with", "this",
    "that", "quote", "comment", "please", "keep", "short", "about", "find", "news", "article",
];

/// Brave query: subject keywords, space = AND. No instruction filler.
#[must_use]
pub fn web_search_query(brief: &str) -> String {
    let k = search_keywords(brief);
    if k.is_empty() {
        "technology".into()
    } else {
        k.join(" ")
    }
}

/// X query: first keyword AND (OR of the rest), e.g. `Alpha (Beta OR Gamma)`.
#[must_use]
pub fn x_search_query(brief: &str) -> String {
    let k = search_keywords(brief);
    match k.as_slice() {
        [] => "technology".into(),
        [one] => one.clone(),
        [head, rest @ ..] => format!("{head} ({})", rest.join(" OR ")),
    }
}

/// True when the writer dumped a FAQ / `ResearchPack` essay instead of an X tweet.
#[must_use]
pub fn tweet_body_exploded(raw: &str) -> bool {
    let text = crate::publish::tweet_text_for_api(raw);
    if text.chars().count() > 500 {
        return true;
    }
    let nonempty = text.lines().filter(|l| !l.trim().is_empty()).count();
    if nonempty > 8 {
        return true;
    }
    let l = text.to_ascii_lowercase();
    l.contains("based on the information provided")
        || l.contains("based on the provided information")
        || l.contains("what should you do next")
        || l.contains("curated researchpack")
        || l.contains("recommended reading order")
        || text.contains("\n## ")
        || text.starts_with("## ")
        || text.contains("\n### ")
        || text.starts_with("### ")
}

/// Blank line between sentence beats when the writer returned one dense line.
#[must_use]
pub fn aerate_tweet_commentary(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() || text.contains('\n') {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut buf = String::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        buf.push(ch);
        let end_sent = matches!(ch, '.' | '!' | '?');
        let ellipsis = end_sent && chars.get(i + 1) == Some(&'.');
        if end_sent && !ellipsis && chars.get(i + 1) == Some(&' ') {
            let next = chars.get(i + 2).copied();
            if next.is_some_and(|c| c.is_uppercase() || !c.is_ascii()) {
                let beat = buf.trim();
                if !beat.is_empty() {
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    out.push_str(beat);
                }
                buf.clear();
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    let rest = buf.trim();
    if !rest.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(rest);
    }
    if out.is_empty() {
        text.to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn tweet_ids_monotonic_and_shaped() {
        let dir = TempDir::new().expect("temp");
        let path = dir.path().join("s.db");
        let a = next_tweet_id(&path).expect("a");
        let b = next_tweet_id(&path).expect("b");
        assert!(a.starts_with("TWEET-"));
        assert!(a.contains(&Local::now().format("%Y%m%d").to_string()));
        assert!(a.ends_with("-000001"));
        assert!(b.ends_with("-000002"));
        assert_ne!(a, b);
    }

    #[test]
    fn compose_lists_all_three_including_x() {
        let body = ensure_tweet_cite_line(
            "Builders: the owl spotted a merge.\n",
            Some("https://hotpath.rs/blog/profiling-rust-guide"),
        );
        let cites = vec![
            "https://x.com/ayushagarwal027/status/2087899096606761217".into(),
            "https://hotpath.rs/blog/profiling-rust-guide".into(),
            "https://hotpath.rs/blog/rust-performance-profiling".into(),
        ];
        let out = compose_tweet_message(&body, "TWEET-20260814-000016", &cites);
        assert!(out.contains("Link: 2"), "{out}");
        assert!(out.contains("1. https://x.com/ayushagarwal027/status/2087899096606761217"));
        assert!(out.contains("2. https://hotpath.rs/blog/profiling-rust-guide"));
        assert!(out.contains("3. https://hotpath.rs/blog/rust-performance-profiling"));
        assert!(!out.contains("Cite"));
        assert!(!out.contains("\nX:"));
        assert_eq!(
            quote_tweet_id_from_cites(&cites).as_deref(),
            Some("2087899096606761217")
        );
    }

    #[test]
    fn compose_keeps_one_body_ship_splits_when_over_280() {
        let body = "\
🦉 GitHub Models' retirement feels like a quiet end to a promising experiment.
Sad to see such a tool fade-especially when free alternatives are scarce.

Microsoft Foundry and Copilot?
Not exactly the open-source dream we hoped for.

Builders, keep an eye on migration paths.
The future of AI tools is still in flux.

#AI #GitHub #ModelRetirement

https://blog.dante.company/en/articles/github-models-retirement-migration-2026-07-02";
        let cite =
            "https://blog.dante.company/en/articles/github-models-retirement-migration-2026-07-02";
        let out = compose_tweet_message(body, "TWEET-20260814-000010", &[cite.into()]);
        assert!(!out.contains("1/2"));
        assert!(!out.contains("2/2"));
        assert!(out.contains("Link: 1"));
        let texts = crate::publish::tweet_texts_for_api(&out);
        assert_eq!(texts.len(), 2);
        assert!(crate::sources::tweet_thread::fits_x_limit(&texts[0]));
        assert!(crate::sources::tweet_thread::fits_x_limit(&texts[1]));
        assert!(!texts[0].contains("blog.dante.company"));
        assert!(texts[1].contains("#AI"));
        assert!(texts[1].contains(cite));
    }

    #[test]
    fn ensure_publisher_inserts_link_strips_status() {
        let body = "Hello\n\nhttps://x.com/a/status/1\n\nSources: none";
        let out = ensure_tweet_cite_line(body, Some("https://labs.sogeti.com/a"));
        assert!(out.contains("https://labs.sogeti.com/a"));
        assert!(!out
            .split("Sources:")
            .next()
            .unwrap()
            .contains("x.com/a/status"));
        let quoted = ensure_tweet_cite_line(body, None);
        assert!(!quoted
            .split("Sources:")
            .next()
            .unwrap()
            .contains("https://x.com"));
    }

    #[test]
    fn quote_plus_publisher_first_option_is_body_link() {
        let body = "Crab game looks fun.\n\nhttps://x.com/haipingfu/status/1\n";
        let cites = vec![
            "https://x.com/haipingfu/status/1".into(),
            "https://store.steampowered.com/app/1182480".into(),
        ];
        let out = ensure_tweet_cite_line(body, in_tweet_publisher_url(&cites));
        let head = out.split("Sources:").next().unwrap_or(out.as_str());
        assert!(head.contains("x.com/haipingfu"));
        assert!(!head.contains("https://store.steampowered.com/app/1182480"));
        let composed = compose_tweet_message(&out, "TWEET-20260814-000015", &cites);
        assert!(composed.contains("Link: 1"));
        assert!(composed.contains("1. https://x.com/haipingfu/status/1"));
        assert!(composed.contains("2. https://store.steampowered.com/app/1182480"));
        assert!(!composed.contains("Cite"));
        assert!(crate::publish::tweet_text_for_api(&composed).contains("x.com/haipingfu"));
    }

    #[test]
    fn ensure_replaces_existing_publisher_url() {
        let body = "ITCy is a Linux owl.\n\nhttps://interchouette.net/\n";
        let out =
            ensure_tweet_cite_line(body, Some("https://github.com/Interchouette-ITC/itcy-tui"));
        let head = out.split("Link:").next().unwrap_or(out.as_str());
        assert!(head.contains("https://github.com/Interchouette-ITC/itcy-tui"));
        assert!(!head.contains("https://interchouette.net/"));
        assert_eq!(
            head.lines()
                .filter(|l| l.trim().starts_with("https://"))
                .count(),
            1
        );
    }

    #[test]
    fn ensure_clear_removes_publisher_url() {
        let body = "Hello\n\nhttps://interchouette.net/\n";
        let out = ensure_tweet_cite_line(body, None);
        assert!(!out.contains("https://"));
        assert!(out.contains("Hello"));
    }

    #[test]
    fn change_path_body_matches_link_one() {
        let body = "Builders.\n\nhttps://interchouette.net/";
        let new_url = "https://github.com/Interchouette-ITC/itcy-tui";
        let mut opts = vec![
            "https://interchouette.net/".into(),
            "https://interchouette.net/CV/".into(),
            new_url.into(),
        ];
        crate::sources::draft_url::promote_link_option(&mut opts, new_url);
        let rewritten = ensure_tweet_cite_line(body, Some(new_url));
        let composed = compose_tweet_message(&rewritten, "TWEET-20260813-000002", &opts);
        let api = crate::publish::tweet_text_for_api(&composed);
        assert!(
            api.contains(new_url),
            "shipped text must carry new link: {api}"
        );
        assert!(
            !api.contains("https://interchouette.net/"),
            "old link must leave body: {api}"
        );
        assert!(composed.contains(&format!("1. {new_url}")));
        assert!(composed.contains("Link: 1"));
    }

    #[test]
    fn apply_x_status_url_puts_x_in_tweet_as_link_one() {
        let body = "\
Tweet ID: TWEET-20260814-000016

Optimizer magic.

https://hotpath.rs/blog/profiling-rust-guide

Link: 2
0 = no link. /change_url TWEET-20260814-000016 <0|1|2|3|url>
1. https://x.com/ayushagarwal027/status/2087899096606761217
2. https://hotpath.rs/blog/profiling-rust-guide
3. https://hotpath.rs/blog/rust-performance-profiling";
        let opts = vec![
            "https://x.com/ayushagarwal027/status/2087899096606761217".into(),
            "https://hotpath.rs/blog/profiling-rust-guide".into(),
            "https://hotpath.rs/blog/rust-performance-profiling".into(),
        ];
        let x = "https://x.com/ayushagarwal027/status/2087899096606761217";
        let (out, options) =
            apply_change_tweet_url("TWEET-20260814-000016", body, &opts, x).expect("apply");
        assert!(out.contains(&format!("1. {x}")));
        assert!(out.contains("2. https://hotpath.rs/blog/profiling-rust-guide"));
        assert!(out.contains("3. https://hotpath.rs/blog/rust-performance-profiling"));
        assert!(out.contains("Link: 1"));
        assert_eq!(options[0], x);
        assert!(crate::publish::tweet_text_for_api(&out).contains(x));
        assert!(!crate::publish::tweet_text_for_api(&out).contains("hotpath.rs"));
        assert_eq!(
            quote_tweet_id_from_cites(&options).as_deref(),
            Some("2087899096606761217")
        );
    }

    #[test]
    fn apply_index_promotes_choice_to_link_one() {
        let body = "Hello\n\nhttps://hotpath.rs/blog/profiling-rust-guide\n";
        let opts = vec![
            "https://x.com/a/status/1".into(),
            "https://hotpath.rs/blog/profiling-rust-guide".into(),
            "https://hotpath.rs/blog/rust-performance-profiling".into(),
        ];
        let (out, options) = apply_change_tweet_url("TWEET-1", body, &opts, "3").expect("apply");
        assert!(out.contains("Link: 1"));
        assert_eq!(
            options[0],
            "https://hotpath.rs/blog/rust-performance-profiling"
        );
        assert!(out.contains("1. https://hotpath.rs/blog/rust-performance-profiling"));
        assert!(crate::publish::tweet_text_for_api(&out)
            .contains("https://hotpath.rs/blog/rust-performance-profiling"));
    }

    #[test]
    fn apply_zero_clears_link_keeps_all_options() {
        let body = "Hello\n\nhttps://hotpath.rs/blog/a\n";
        let opts = vec![
            "https://x.com/a/status/9".into(),
            "https://hotpath.rs/blog/a".into(),
            "https://hotpath.rs/blog/b".into(),
        ];
        let (out, options) = apply_change_tweet_url("TWEET-1", body, &opts, "0").expect("clear");
        assert!(out.contains("Link: 0"));
        assert!(out.contains("1. https://x.com/a/status/9"));
        assert!(out.contains("2. https://hotpath.rs/blog/a"));
        assert_eq!(options, opts);
        assert!(!crate::publish::tweet_text_for_api(&out).contains("https://"));
    }

    #[test]
    fn apply_custom_https_sets_cite_and_footer_command() {
        let body = "Hello\n\nhttps://mcp.interchouette.net\n";
        let opts = vec!["https://mcp.interchouette.net".into()];
        let url = "https://www.spronta.com/blog/state-of-webmcp-july-2026";
        let (out, options) = apply_change_tweet_url("TWEET-1", body, &opts, url).expect("apply");
        assert_eq!(options[0], url);
        assert!(out.contains(url));
        assert!(out.contains("/change_url TWEET-1"));
        assert!(!out.contains("/change_tweet_url"));
    }

    #[test]
    fn apply_index_one_puts_x_in_body() {
        let body = "Hello\n\nhttps://hotpath.rs/blog/a\n";
        let opts = vec![
            "https://x.com/a/status/9".into(),
            "https://hotpath.rs/blog/a".into(),
        ];
        let (out, options) = apply_change_tweet_url("TWEET-1", body, &opts, "1").expect("x");
        assert!(out.contains("Link: 1"));
        assert_eq!(options[0], "https://x.com/a/status/9");
        assert!(crate::publish::tweet_text_for_api(&out).contains("x.com/a/status/9"));
        assert!(!crate::publish::tweet_text_for_api(&out).contains("hotpath.rs"));
    }

    #[test]
    fn extract_brief_cite_from_instructions_url() {
        let u = extract_brief_cite(
            "Casper vs RWA and x402 payments, quote this and comment https://x.com/Casper_Network/status/2088223231035551912",
        );
        assert_eq!(
            u.as_deref(),
            Some("https://x.com/Casper_Network/status/2088223231035551912")
        );
        let still = extract_brief_cite(
            "Casper vs RWA, quote this Prefer cite: https://x.com/Casper_Network/status/2088223231035551912",
        );
        assert_eq!(
            still.as_deref(),
            Some("https://x.com/Casper_Network/status/2088223231035551912")
        );
        assert_eq!(
            search_keywords(
                "Casper vs RWA and x402 payments, quote this and comment https://x.com/a/status/1"
            ),
            vec!["Casper", "RWA", "x402", "payments"]
        );
        assert_eq!(
            web_search_query(
                "Casper vs RWA and x402 payments, quote this and comment https://x.com/a/status/1"
            ),
            "Casper RWA x402 payments"
        );
        assert_eq!(
            x_search_query(
                "Casper vs RWA and x402 payments, quote this and comment https://x.com/a/status/1"
            ),
            "Casper (RWA OR x402 OR payments)"
        );
    }

    #[test]
    fn exploded_researchpack_dump_is_rejected() {
        assert!(tweet_body_exploded(
            "Based on the provided information, here’s a **curated ResearchPack** focusing on x402.\n\n\
### ResearchPack: x402\n\n\
#### 1. Protocol\n\n\
Recommended reading order next.\n"
        ));
        assert!(!tweet_body_exploded(
            "Agents still cannot pay. Casper’s x402 bet is the part worth quoting.\n"
        ));
    }

    #[test]
    fn aerates_dense_one_liner_into_blank_separated_beats() {
        let out = aerate_tweet_commentary("Hello! I’m ITCy. Let’s build something fun.");
        assert_eq!(out, "Hello!\n\nI’m ITCy.\n\nLet’s build something fun.");
        let already = "Hello!\n\nI’m ITCy.";
        assert_eq!(aerate_tweet_commentary(already), already);
    }
}

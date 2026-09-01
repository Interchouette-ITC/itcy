// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! HTTP reachability probe for publisher cites (reject 404 / soft-not-found / empty shells).

use crate::sources::draft_footer::ensure_primary_link_line;
use crate::sources::draft_url::{extract_in_post_url, set_single_in_post_url};
use crate::sources::html::extract_articleish_text;
use crate::sources::tweet_footer::extract_brief_cite;
use crate::sources::url_hygiene::{
    is_allowed_tweet_cite, is_junk_or_search_url, is_x_status_url, same_publisher_domain,
    scrub_https_url,
};
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{info, warn};

const PROBE_BODY_CAP: usize = 256_000;
/// Publisher cite probe floor (browse/ingest may still require [`MIN_STORE_CHARS`]).
const PROBE_MIN_ARTICLE_CHARS: usize = 120;

/// Floor: refill until at least this many reachable Link options (when the pool allows).
pub const LINK_OPTIONS_MIN: usize = 3;
/// Ceiling: operator may keep up to this many Link slots (3 is the floor, not the cap).
pub const LINK_OPTIONS_CAP: usize = 5;

const NOT_FOUND_HTML_MARKERS: &[&str] = &[
    "404 - file or directory not found",
    "this page doesn't exist",
    "this page does not exist",
    "sorry, we couldn't find that page",
    "sorry, we could not find that page",
    "page not found",
];

/// True when HTML/title text looks like a not-found page (not an article about 404s).
#[must_use]
pub fn html_page_looks_like_not_found(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    if NOT_FOUND_HTML_MARKERS.iter().any(|m| lower.contains(m)) {
        return true;
    }
    if let Some(title) = extract_html_title(&lower) {
        if title.contains("404")
            || title.contains("not found")
            || title.contains("page doesn't exist")
            || title.contains("page does not exist")
        {
            return true;
        }
    }
    false
}

fn extract_html_title(lower_html: &str) -> Option<String> {
    let start = lower_html.find("<title")?;
    let after = &lower_html[start..];
    let gt = after.find('>')? + 1;
    let rest = &after[gt..];
    let end = rest.find("</title>")?;
    Some(rest[..end].trim().to_string())
}

/// Evaluate HTTP status + HTML without network (unit tests).
///
/// # Errors
///
/// Returns a short reason when the URL must not ship as a publisher cite.
pub fn evaluate_publisher_probe(status: u16, body: &str) -> Result<(), String> {
    if status == 404 || status == 410 {
        return Err(format!("HTTP {status}"));
    }
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}"));
    }
    if html_page_looks_like_not_found(body) {
        return Err("page looks like not found".into());
    }
    // Require a real article/main/Apollo body. Fat Next.js shells return 200 with
    // nav chrome only; full-page strip would pass MIN_STORE_CHARS and lie.
    let Some(text) = extract_articleish_text(body) else {
        return Err("page has no article body".into());
    };
    let chars = text.chars().count();
    if chars < PROBE_MIN_ARTICLE_CHARS {
        return Err(format!("page too thin ({chars} chars)"));
    }
    Ok(())
}

fn skip_publisher_url_probe(url: &str) -> bool {
    // X status cites are validated via Twitter API / browse, not raw GET (bot walls).
    if is_x_status_url(url) {
        return true;
    }
    // Unit-test fixture hosts (never live DNS); keep Link floor tests offline.
    cfg!(test) && url.to_ascii_lowercase().contains(".itcy.test/")
}

fn is_loopback_probe_url(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.starts_with("http://127.")
        || u.starts_with("http://localhost")
        || u.starts_with("http://[::1]")
}

fn probe_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent("ITCy/0.1 (+https://interchouette.net; publisher URL probe)")
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// GET probe: reject dead publisher URLs before they land in Link options or ship.
///
/// # Errors
///
/// Returns a short reason (HTTP code, thin page, soft 404, empty shell).
pub async fn probe_publisher_url(url: &str) -> Result<(), String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("not an https URL".into());
    }
    let loopback_test = cfg!(test) && is_loopback_probe_url(url);
    if !url.starts_with("https://") && !loopback_test {
        return Err("not an https URL".into());
    }
    if skip_publisher_url_probe(url) {
        return Ok(());
    }
    let client = probe_http_client();
    let res = client.get(url).send().await.map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    let mut body = res.text().await.map_err(|e| e.to_string())?;
    truncate_utf8_bytes(&mut body, PROBE_BODY_CAP);
    evaluate_publisher_probe(status, &body)
}

/// Cap HTTP body by bytes without slicing mid-UTF-8 (panic on `String::truncate`).
fn truncate_utf8_bytes(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Keep only publisher URLs that respond with real article content.
#[must_use]
pub async fn filter_reachable_publisher_urls(urls: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for u in urls {
        // Never probe prose tokens / SERP chrome (`https://SUBJECT`, Brave search, …).
        // Unit tests use `http://127.0.0.1` fixtures (same exception as refill).
        let loopback_test = cfg!(test) && is_loopback_probe_url(&u);
        if !is_x_status_url(&u) && !is_allowed_tweet_cite(&u) && !loopback_test {
            warn!(url = %u, "publisher_url: dropped non-cite before probe");
            continue;
        }
        if skip_publisher_url_probe(&u) {
            out.push(u);
            continue;
        }
        match probe_publisher_url(&u).await {
            Ok(()) => {
                info!(url = %u, "publisher_url: kept after probe");
                out.push(u);
            }
            Err(e) => warn!(url = %u, error = %e, "publisher_url: dropped unreachable"),
        }
    }
    out
}

/// Drop dead Link options, refill from `pool` up to [`LINK_OPTIONS_MIN`], sync in-post cite.
#[must_use]
pub async fn finalize_reachable_link_options(
    body: &str,
    link_options: Vec<String>,
) -> (String, Vec<String>) {
    finalize_reachable_link_options_from_pool(body, link_options, &[]).await
}

/// Like [`finalize_reachable_link_options`], then probe `pool` until [`LINK_OPTIONS_MIN`] fills.
/// Keeps up to [`LINK_OPTIONS_CAP`] reachable options when the pack already has more.
#[must_use]
pub async fn finalize_reachable_link_options_from_pool(
    body: &str,
    link_options: Vec<String>,
    pool: &[String],
) -> (String, Vec<String>) {
    let mut options = filter_reachable_publisher_urls(link_options).await;
    if options.len() > LINK_OPTIONS_CAP {
        options.truncate(LINK_OPTIONS_CAP);
    }
    if options.len() < LINK_OPTIONS_CAP && !pool.is_empty() {
        let before = options.len();
        refill_link_options_from_pool(&mut options, pool).await;
        if options.len() > before {
            info!(
                before,
                after = options.len(),
                "publisher_url: refilled Link options from SERP pool"
            );
        }
    }
    if options.len() < LINK_OPTIONS_MIN {
        warn!(
            n = options.len(),
            min = LINK_OPTIONS_MIN,
            "publisher_url: fewer than 3 reachable Link options after probe+refill"
        );
    }
    let primary = options.first().map(String::as_str);
    let mut prose = set_single_in_post_url(body, primary.unwrap_or(""));
    prose = ensure_primary_link_line(&prose, primary);
    (prose, options)
}

async fn refill_link_options_from_pool(options: &mut Vec<String>, pool: &[String]) {
    for raw in pool {
        if options.len() >= LINK_OPTIONS_CAP {
            break;
        }
        let scrubbed = scrub_https_url(raw);
        let loopback_test = cfg!(test) && is_loopback_probe_url(&scrubbed);
        if scrubbed.is_empty() {
            continue;
        }
        if is_junk_or_search_url(&scrubbed) && !loopback_test {
            continue;
        }
        if !is_allowed_tweet_cite(&scrubbed) && !is_x_status_url(&scrubbed) && !loopback_test {
            continue;
        }
        if options
            .iter()
            .any(|u| same_publisher_domain(u, &scrubbed) || u == &scrubbed)
        {
            continue;
        }
        if skip_publisher_url_probe(&scrubbed) {
            options.push(scrubbed);
            continue;
        }
        match probe_publisher_url(&scrubbed).await {
            Ok(()) => options.push(scrubbed),
            Err(e) => warn!(url = %scrubbed, error = %e, "publisher_url: pool candidate dropped"),
        }
    }
}

/// Gate BAT `/accept`: ship cite must exist and be reachable (not a 3-link build rule).
///
/// # Errors
///
/// Returns operator-facing text when there is no cite, or the in-post / Link:1 URL is unreachable.
pub async fn require_ship_cite_reachable(
    body: &str,
    link_options: &[String],
) -> Result<(), String> {
    require_ship_cite_url_reachable(body, link_options).await
}

/// Tweet BAT accept: same ship-cite gate as [`require_ship_cite_reachable`] (no link count).
///
/// # Errors
///
/// Same as [`require_ship_cite_reachable`].
pub async fn require_tweet_ship_cite_reachable(
    _brief: &str,
    body: &str,
    link_options: &[String],
) -> Result<(), String> {
    require_ship_cite_url_reachable(body, link_options).await
}

async fn require_ship_cite_url_reachable(
    body: &str,
    link_options: &[String],
) -> Result<(), String> {
    let url = extract_in_post_url(body)
        .or_else(|| link_options.first().cloned())
        .filter(|u| !u.trim().is_empty());
    let Some(url) = url else {
        return Err(
            "No publisher Link to ship. Pick Link:1 with `/change_url`, or `/rework` with a cite."
                .into(),
        );
    };
    if skip_publisher_url_probe(&url) {
        return Ok(());
    }
    probe_publisher_url(&url).await.map_err(|e| {
        format!(
            "Link not reachable: {url} ({e}). Fix with `/change_url` or `/rework`, then `/accept`."
        )
    })
}

/// Hard floor: drafts/tweets must keep at least [`LINK_OPTIONS_MIN`] reachable publisher URLs.
///
/// Soft-warn alone let DRAFT-20260831-000137 save with `Link: 0` after scheme-only SERP junk.
///
/// # Errors
///
/// Returns operator-facing text when the floor is missed.
pub fn require_link_options_floor(link_options: &[String]) -> Result<(), String> {
    require_link_options_floor_min(LINK_OPTIONS_MIN, link_options, None)
}

/// Tweet floor: locked X status cite needs only the operator URL; publisher cites need three.
///
/// # Errors
///
/// Returns operator-facing text when the floor is missed.
pub fn require_tweet_link_options_floor(
    brief: &str,
    link_options: &[String],
) -> Result<(), String> {
    let min = tweet_link_options_min(brief, link_options);
    let hint = if min == 1 {
        Some(
            "Operator locked an X status cite; Link:1 is the quote card. \
Add a publisher https in the brief for extra Link options.",
        )
    } else {
        None
    };
    require_link_options_floor_min(min, link_options, hint)
}

/// Minimum Link options for tweets: 1 when operator brief locks an X status cite.
#[must_use]
pub fn tweet_link_options_min(brief: &str, link_options: &[String]) -> usize {
    let _ = link_options;
    if extract_brief_cite(brief).is_some_and(|u| is_x_status_url(&u)) {
        1
    } else {
        LINK_OPTIONS_MIN
    }
}

fn require_link_options_floor_min(
    min: usize,
    link_options: &[String],
    hint: Option<&str>,
) -> Result<(), String> {
    if link_options.len() < min {
        let mut msg = format!(
            "Need at least {min} reachable publisher Link options (got {}). \
Refuse draft/tweet with Link:0. Retry with a live publisher URL, or `/draft_about` / `/tweet_about` with a cite.",
            link_options.len()
        );
        if let Some(h) = hint {
            msg.push(' ');
            msg.push_str(h);
        }
        return Err(msg);
    }
    if link_options
        .iter()
        .any(|u| !crate::sources::url_hygiene::host_looks_like_dns(u))
    {
        return Err(
            "Link options contain scheme-only or non-DNS junk (e.g. `http://`). Refuse ship."
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    use std::net::SocketAddr;

    #[test]
    fn evaluate_rejects_http_404() {
        let err = evaluate_publisher_probe(404, "").expect_err("404");
        assert!(err.contains("404"), "{err}");
    }

    #[test]
    fn evaluate_rejects_soft_not_found_title() {
        let html = "<html><head><title>404: This page could not be found</title></head>\
            <body><h1>Not found</h1></body></html>";
        let err = evaluate_publisher_probe(200, html).expect_err("soft 404");
        assert!(err.contains("not found") || err.contains("404"), "{err}");
    }

    #[test]
    fn evaluate_accepts_article_html() {
        let body = "word ".repeat(120);
        let html = format!(
            "<html><head><title>News</title></head><body><article>{body}</article></body></html>"
        );
        evaluate_publisher_probe(200, &html).expect("ok article");
    }

    #[test]
    fn evaluate_rejects_fat_spa_shell_without_article() {
        let chrome = "nav word ".repeat(80);
        let html = format!(
            "<html><head><title>App</title></head><body><div id=\"root\">{chrome}</div></body></html>"
        );
        let err = evaluate_publisher_probe(200, &html).expect_err("empty shell");
        assert!(err.contains("no article") || err.contains("thin"), "{err}");
    }

    #[test]
    fn vercel_ai_sdk_security_slug_would_fail_probe() {
        let err = evaluate_publisher_probe(404, "<html><body>404</body></html>")
            .expect_err("broken vercel slug pattern");
        assert!(err.contains("404"), "{err}");
    }

    #[test]
    fn truncate_utf8_bytes_does_not_panic_mid_multibyte() {
        // DRAFT-20260831-000135: probe body.truncate(cap) panicked on is_char_boundary
        // when a multi-byte glyph straddled PROBE_BODY_CAP.
        let emoji = "🦀"; // 4 bytes
        assert_eq!(emoji.len(), 4);
        let mut s = "a".repeat(10);
        s.push_str(emoji);
        s.push_str(&"b".repeat(10));
        // Cap lands inside the emoji (byte 11 of "a"*10 + first byte of 🦀).
        truncate_utf8_bytes(&mut s, 11);
        assert_eq!(s, "aaaaaaaaaa");
        assert!(s.is_char_boundary(s.len()));
    }

    #[tokio::test]
    async fn filter_drops_unreachable_localhost_url() {
        let app = Router::new().route(
            "/dead",
            get(|| async { (axum::http::StatusCode::NOT_FOUND, "gone") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr: SocketAddr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let dead = format!("http://{addr}/dead");
        let out = filter_reachable_publisher_urls(vec![dead]).await;
        assert!(out.is_empty(), "404 must not survive filter");
    }

    #[tokio::test]
    async fn filter_keeps_reachable_localhost_url() {
        let body = "word ".repeat(120);
        let app = Router::new().route(
            "/ok",
            get(move || {
                let b = body.clone();
                async move {
                    (
                        axum::http::StatusCode::OK,
                        format!("<html><body><article>{b}</article></body></html>"),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr: SocketAddr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let ok = format!("http://{addr}/ok");
        let out = filter_reachable_publisher_urls(vec![ok.clone()]).await;
        assert_eq!(out, vec![ok]);
    }

    #[tokio::test]
    async fn finalize_clears_in_post_when_all_links_dead() {
        let app = Router::new().route(
            "/dead",
            get(|| async { (axum::http::StatusCode::NOT_FOUND, "gone") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr: SocketAddr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let dead = format!("http://{addr}/dead");
        let body = format!("Post prose.\n\n{dead}\n");
        let (_prose, opts) = finalize_reachable_link_options(&body, vec![dead.clone()]).await;
        assert!(opts.is_empty(), "dead link must not remain in Link options");
    }

    #[tokio::test]
    async fn finalize_refills_from_pool_after_empty_shell_probe() {
        let body_ok = "word ".repeat(120);
        let app = Router::new()
            .route(
                "/shell",
                get(|| async {
                    (
                        axum::http::StatusCode::OK,
                        "<html><body><div id=\"root\">nav only</div></body></html>".to_string(),
                    )
                }),
            )
            .route(
                "/ok",
                get(move || {
                    let b = body_ok.clone();
                    async move {
                        (
                            axum::http::StatusCode::OK,
                            format!("<html><body><article>{b}</article></body></html>"),
                        )
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr: SocketAddr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let shell = format!("http://{addr}/shell");
        let ok = format!("http://{addr}/ok");
        let (_prose, opts) = finalize_reachable_link_options_from_pool(
            "Post.\n",
            vec![shell.clone()],
            std::slice::from_ref(&ok),
        )
        .await;
        assert!(
            !opts.iter().any(|u| u == &shell),
            "empty shell must not stay: {opts:?}"
        );
        assert_eq!(opts, vec![ok], "pool article must refill Link options");
    }

    #[test]
    fn require_link_options_floor_rejects_empty_and_scheme_only() {
        // DRAFT-20260831-000137: soft-warn alone saved Link:0 after http:// / https:// SERP junk.
        assert!(require_link_options_floor(&[]).is_err());
        assert!(require_link_options_floor(&[
            "http://".into(),
            "https://".into(),
            "https://".into(),
        ])
        .is_err());
        let three = vec![
            "https://labs.sogeti.com/a".into(),
            "https://decrypt.co/1".into(),
            "https://techcrunch.com/c".into(),
        ];
        assert!(require_link_options_floor(&three).is_ok());
        assert!(require_link_options_floor(&three[..2]).is_err());
    }

    #[test]
    fn tweet_link_floor_one_when_operator_locked_x_status_cite() {
        let x = "https://x.com/nineshoot/status/2094567713113059575";
        let brief = format!("Obscura Rust browser, Short punchy take, Link cite {x}");
        let one = vec![x.to_string()];
        assert!(require_tweet_link_options_floor(&brief, &one).is_ok());
        assert!(require_link_options_floor(&one).is_err());
        let publisher_brief = "Obscura Rust browser, cite https://labs.sogeti.com/obscura";
        assert!(require_tweet_link_options_floor(publisher_brief, &one).is_err());
    }

    #[test]
    fn propose_serp_junk_cannot_satisfy_link_floor() {
        let propose =
            "Propose one company-page LinkedIn post from corpus memory on this subject.\n\n\
Corpus grounding:\ncontext quality mush";
        let q = crate::sources::tweet_footer::web_search_query(
            "agentic coding 2026 practical guide big",
            propose,
        );
        assert_eq!(q, "agentic coding 2026 practical guide big");
        let extracted = crate::sources::url_hygiene::filter_publisher_urls(&[
            "http://".into(),
            "https://".into(),
        ]);
        assert!(
            extracted.is_empty(),
            "scheme-only EXTRACTED must not enter pack: {extracted:?}"
        );
        assert!(require_link_options_floor(&extracted).is_err());
    }

    #[tokio::test]
    async fn require_ship_cite_rejects_empty_link_options() {
        let err = require_ship_cite_reachable("Prose only.\n", &[])
            .await
            .expect_err("empty options must fail accept");
        assert!(
            err.contains("No publisher Link") || err.contains("Link options"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn accept_ship_one_x_link_option_without_three_in_pool() {
        let x = "https://x.com/nineshoot/status/2094567713113059575";
        let body = format!("Commentary beats.\n\n#Rust\n\n{x}\n");
        let opts = vec![x.to_string()];
        require_ship_cite_reachable(&body, &opts)
            .await
            .expect("accept only checks ship cite, not link count");
    }
}

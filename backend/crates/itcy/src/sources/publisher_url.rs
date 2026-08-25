// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! HTTP reachability probe for publisher cites (reject 404 / soft-not-found before ship).

use crate::sources::draft_footer::ensure_primary_link_line;
use crate::sources::draft_url::{extract_in_post_url, set_single_in_post_url};
use crate::sources::html::extract_page_text;
use crate::sources::ingest::MIN_STORE_CHARS;
use crate::sources::url_hygiene::is_x_status_url;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::warn;

const PROBE_BODY_CAP: usize = 256_000;

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
    let text = extract_page_text(body);
    let chars = text.chars().count();
    if chars < MIN_STORE_CHARS {
        return Err(format!("page too thin ({chars} chars)"));
    }
    Ok(())
}

fn skip_publisher_url_probe(url: &str) -> bool {
    // X status cites are validated via Twitter API / browse, not raw GET (bot walls).
    is_x_status_url(url)
}

fn is_loopback_probe_url(url: &str) -> bool {
    url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")
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
/// Returns a short reason (HTTP code, thin page, soft 404).
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
    if body.len() > PROBE_BODY_CAP {
        body.truncate(PROBE_BODY_CAP);
    }
    evaluate_publisher_probe(status, &body)
}

/// Keep only publisher URLs that respond with real content.
#[must_use]
pub async fn filter_reachable_publisher_urls(urls: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for u in urls {
        if skip_publisher_url_probe(&u) {
            out.push(u);
            continue;
        }
        match probe_publisher_url(&u).await {
            Ok(()) => out.push(u),
            Err(e) => warn!(url = %u, error = %e, "publisher_url: dropped unreachable"),
        }
    }
    out
}

/// Drop dead Link options and sync the in-post https line to the first survivor.
#[must_use]
pub async fn finalize_reachable_link_options(
    body: &str,
    link_options: Vec<String>,
) -> (String, Vec<String>) {
    let options = filter_reachable_publisher_urls(link_options).await;
    let primary = options.first().map(String::as_str);
    let mut prose = set_single_in_post_url(body, primary.unwrap_or(""));
    prose = ensure_primary_link_line(&prose, primary);
    (prose, options)
}

/// Gate BAT accept: the cite that ships must not 404.
///
/// # Errors
///
/// Returns operator-facing text when the in-post / Link:1 URL is unreachable.
pub async fn require_ship_cite_reachable(
    body: &str,
    link_options: &[String],
) -> Result<(), String> {
    let url = extract_in_post_url(body)
        .or_else(|| link_options.first().cloned())
        .filter(|u| !u.trim().is_empty());
    let Some(url) = url else {
        return Ok(());
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
    fn vercel_ai_sdk_security_slug_would_fail_probe() {
        let err = evaluate_publisher_probe(404, "<html><body>404</body></html>")
            .expect_err("broken vercel slug pattern");
        assert!(err.contains("404"), "{err}");
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
}

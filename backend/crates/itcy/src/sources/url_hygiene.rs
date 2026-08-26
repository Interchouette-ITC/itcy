// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Publisher URL hygiene: drop placeholders, SERP, social, shorteners.

/// Public X handle used for status URLs and own-handle scrub.
pub const X_PUBLIC_HANDLE: &str = "Interchouette";

/// X API v2 root (`…/2`).
pub const TWITTER_API_V2_BASE: &str = "https://api.twitter.com/2";

/// `LinkedIn` Community Management posts endpoint.
pub const LINKEDIN_REST_POSTS_URL: &str = "https://api.linkedin.com/rest/posts";

/// Public `https://x.com/{handle}/status/{id}` for a shipped status id.
#[must_use]
pub fn x_status_public_url(status_id: &str) -> String {
    format!("https://x.com/{X_PUBLIC_HANDLE}/status/{status_id}")
}

/// True when the URL points at `LinkedIn` (`linkedin.com` or `lnkd.in`).
#[must_use]
pub fn is_linkedin_host(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("linkedin.com") || lower.contains("lnkd.in")
}

/// True for `LinkedIn` **Pulse** article URLs (`…/pulse/…` on linkedin.com).
///
/// Clearnet `/ingest` only. Posts / activity stay on `/enrich` (Tor).
#[must_use]
pub fn is_linkedin_pulse_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if !lower.contains("linkedin.com") || lower.contains("lnkd.in") {
        return false;
    }
    let Some(path) = url_path_lower(&lower) else {
        return false;
    };
    path.contains("/pulse/")
}

/// Scheme + host + path only (drop query and fragment) for corpus upsert stability.
#[must_use]
pub fn canonicalize_ingest_url(url: &str) -> String {
    let t = url.trim();
    let (scheme, rest) = if let Some(r) = t.strip_prefix("https://") {
        ("https://", r)
    } else if let Some(r) = t.strip_prefix("http://") {
        ("http://", r)
    } else {
        return t.to_string();
    };
    let path_and_host = rest.split(['?', '#']).next().unwrap_or(rest);
    format!("{scheme}{}", path_and_host.trim_end_matches('/'))
}

fn url_path_lower(url_lower: &str) -> Option<&str> {
    let rest = url_lower
        .strip_prefix("https://")
        .or_else(|| url_lower.strip_prefix("http://"))?;
    let after_host = rest.find('/')?;
    Some(&rest[after_host..])
}

const SHORTENER_HOSTS: &[&str] = &[
    "lnkd.in",
    "bit.ly",
    "t.co",
    "tinyurl.com",
    "buff.ly",
    "ow.ly",
    "rb.gy",
];

/// True for known URL shortener hosts (including `lnkd.in`).
#[must_use]
pub fn is_shortener_url(url: &str) -> bool {
    let l = url.to_ascii_lowercase();
    let Some(host) = url_host(&l) else {
        return false;
    };
    let h = host.strip_prefix("www.").unwrap_or(host);
    SHORTENER_HOSTS.contains(&h)
}

/// True for search / placeholder / social / shortener URLs that must never be cited.
#[must_use]
pub fn is_junk_or_search_url(url: &str) -> bool {
    let l = url.to_ascii_lowercase();
    if is_linkedin_host(&l)
        || l.contains("google.com/search")
        || l.contains("google.com/sorry")
        || l.contains("search.brave.com")
        || l.contains("youtube.com")
        || l.contains("youtu.be")
        || l.contains("reddit.com")
        || l.contains("redd.it")
        || l.contains("raw.githubusercontent.com")
        || l.contains("/example_")
        || l.contains("placeholder")
        || l.contains("iana.org")
        || l.contains("localhost")
        || l.contains("127.0.0.1")
        || is_shortener_url(&l)
        || l.contains("instagram.com")
        || l.contains("facebook.com")
        || l.contains("fb.com/")
        || l.contains("tiktok.com")
    {
        return true;
    }
    host_is_placeholder(&l)
}

/// RFC-ish placeholders and model-invented `example-*` hosts (e.g. example-news-site.com).
fn host_is_placeholder(url_lower: &str) -> bool {
    let Some(host) = url_host(url_lower) else {
        return false;
    };
    let h = host.trim_end_matches('.');
    h == "example.com"
        || h.ends_with(".example.com")
        || h == "example.org"
        || h.ends_with(".example.org")
        || h == "example.net"
        || h.ends_with(".example.net")
        || h.ends_with(".example")
        || h.starts_with("example.")
        || h.contains("example-")
        || h.contains("example_")
}

fn url_host(url_lower: &str) -> Option<&str> {
    let rest = url_lower
        .strip_prefix("https://")
        .or_else(|| url_lower.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.split('@').next_back().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// Drop SERP / placeholder URLs from pack / disclosure labels.
#[must_use]
pub fn filter_publisher_urls(urls: &[String]) -> Vec<String> {
    urls.iter()
        .filter(|u| !is_junk_or_search_url(u))
        .cloned()
        .collect()
}

/// True for an X/Twitter status URL (`x.com/…/status/…` or `twitter.com/…/status/…`).
#[must_use]
pub fn is_x_status_url(url: &str) -> bool {
    x_status_id(url).is_some()
}

/// Numeric status id from an X/Twitter status URL.
#[must_use]
pub fn x_status_id(url: &str) -> Option<String> {
    let lower = url.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let path = rest
        .strip_prefix("x.com/")
        .or_else(|| rest.strip_prefix("twitter.com/"))?;
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let mut parts = path.split('/');
    let first = parts.next()?;
    if first == "i" {
        if parts.next()? != "web" {
            return None;
        }
        if parts.next()? != "status" {
            return None;
        }
    } else if parts.next()? != "status" {
        return None;
    }
    let id = parts.next()?.trim();
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(id.to_string())
}

/// Publisher https **or** X status URL (tweet cite). Drops `LinkedIn` / junk hosts.
#[must_use]
pub fn is_allowed_tweet_cite(url: &str) -> bool {
    let t = url.trim();
    if !t.starts_with("https://") {
        return false;
    }
    // Soft-wrapped scrapes leave a bare `https://` token; never treat it as a cite.
    if url_host(&t.to_ascii_lowercase()).is_none() {
        return false;
    }
    if is_x_status_url(t) {
        return true;
    }
    !is_junk_or_search_url(t) && !is_linkedin_host(t)
}

/// Keep publisher pages and X status URLs for tweet cite options.
#[must_use]
pub fn filter_tweet_cite_urls(urls: &[String]) -> Vec<String> {
    urls.iter()
        .filter(|u| is_allowed_tweet_cite(u))
        .cloned()
        .collect()
}

/// True when `candidate` matches an allowlisted publisher URL (trailing slash / junk tolerant).
#[must_use]
pub fn url_in_allowlist(candidate: &str, allow: &[String]) -> bool {
    let c = normalize_url_key(candidate);
    allow.iter().any(|a| normalize_url_key(a) == c)
}

/// True when two https URLs are the same page (ignore trailing slash / punctuation / case).
#[must_use]
pub fn same_publisher_url(a: &str, b: &str) -> bool {
    !a.trim().is_empty() && normalize_url_key(a) == normalize_url_key(b)
}

/// Hostname for link-option dedup (`www.` stripped, lowercased).
#[must_use]
pub fn publisher_host(url: &str) -> Option<String> {
    let key = normalize_url_key(url);
    url_host(&key).map(|host| host.strip_prefix("www.").unwrap_or(host).to_string())
}

/// True when two https URLs share a publisher host (one slot per domain in link options).
#[must_use]
pub fn same_publisher_domain(a: &str, b: &str) -> bool {
    match (publisher_host(a), publisher_host(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Clean a publisher https for storage / Slack options (drop query, fragment, trailing junk).
#[must_use]
pub fn scrub_https_url(url: &str) -> String {
    let t = url.trim().trim_end_matches(|c: char| {
        matches!(
            c,
            '.' | ',' | ';' | ')' | ']' | '>' | '`' | '"' | '\'' | '*'
        )
    });
    canonicalize_ingest_url(t)
}

/// Unique scrubbed `https://` URLs found in `text` (order preserved).
#[must_use]
pub fn extract_https_urls(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, _) in text.match_indices("https://") {
        let rest = &text[i..];
        let end = rest
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '|' | ')' | ']' | '<' | '`' | '"' | '\'' | '*')
            })
            .unwrap_or(rest.len());
        let scrubbed = scrub_https_url(&rest[..end]);
        if !scrubbed.starts_with("https://") || url_host(&scrubbed.to_ascii_lowercase()).is_none() {
            continue;
        }
        if out.iter().any(|x| same_publisher_url(x, &scrubbed)) {
            continue;
        }
        out.push(scrubbed);
    }
    out
}

/// Dedupe key: scrubbed URL, lowercased.
#[must_use]
pub fn normalize_url_key(url: &str) -> String {
    scrub_https_url(url).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decrypt_co_is_not_shortener_or_junk() {
        let url = "https://decrypt.co/376271/chatgpt-web-ai-written-pew";
        assert!(
            !is_shortener_url(url),
            "decrypt.co must not match t.co substring"
        );
        assert!(!is_junk_or_search_url(url));
        let kept = filter_publisher_urls(&[url.into()]);
        assert_eq!(kept, vec![url.to_string()]);
    }

    #[test]
    fn shortener_host_match_is_host_only() {
        assert!(is_shortener_url("https://t.co/abc123"));
        assert!(is_shortener_url("https://lnkd.in/xyz"));
        assert!(!is_shortener_url(
            "https://decrypt.co/376271/chatgpt-web-ai-written-pew"
        ));
    }

    #[test]
    fn t_co_substring_in_host_or_path_is_not_shortener() {
        // Regression: `decrypt.co` matched `t.co/` via substring and dropped the digest cite.
        let legit = [
            "https://decrypt.co/376271/chatgpt-web-ai-written-pew",
            "https://www.decrypt.co/news/t.co-mentions",
            "https://connect.co/articles/ai",
            "https://www.pewresearch.org/data-labs/t.co/study",
            "https://techcrunch.com/2026/08/20/not-a-shortener",
        ];
        for url in legit {
            assert!(
                !is_shortener_url(url),
                "{url} must not match t.co substring"
            );
            assert!(
                !is_junk_or_search_url(url),
                "{url} must stay in publisher pack"
            );
        }
        assert!(is_shortener_url("https://t.co/abc123"));
        assert!(is_shortener_url("http://t.co/x"));
        assert!(!is_shortener_url("https://evil-t.co.phishing.example/x"));
    }

    #[test]
    fn filter_publisher_urls_drops_shorteners_keeps_decrypt() {
        let urls = vec![
            "https://decrypt.co/376271/chatgpt-web-ai-written-pew".into(),
            "https://t.co/abc123".into(),
            "https://lnkd.in/xyz".into(),
            "https://www.pewresearch.org/data-labs/2026/08/20/how-much-of-the-internet-is-written-with-ai/"
                .into(),
            "https://techcrunch.com/2026/08/20/a-third-of-webpages-published-since-chatgpts-launch-show-signs-of-ai-authorship-study-finds/"
                .into(),
        ];
        let kept = filter_publisher_urls(&urls);
        assert_eq!(kept.len(), 3, "{kept:?}");
        assert!(kept.iter().any(|u| u.contains("decrypt.co")));
        assert!(kept.iter().any(|u| u.contains("pewresearch.org")));
        assert!(kept.iter().any(|u| u.contains("techcrunch.com")));
        assert!(!kept.iter().any(|u| is_shortener_url(u)));
    }

    #[test]
    fn all_shortener_hosts_match_host_only() {
        for host in SHORTENER_HOSTS {
            let url = format!("https://{host}/abc");
            assert!(is_shortener_url(&url), "{url} must be shortener");
        }
        assert!(!is_shortener_url("https://not-bit.ly.evil.example/phish"));
    }

    #[test]
    fn same_publisher_domain_matches_host_not_path() {
        assert!(same_publisher_domain(
            "https://www.pewresearch.org/data-labs/2026/08/20/how-much-of-the-internet-is-written-with-ai/",
            "https://pewresearch.org/data-labs/2026/08/20/methodology-ai-content/"
        ));
        assert!(!same_publisher_domain(
            "https://decrypt.co/376271/chatgpt-web-ai-written-pew",
            "https://www.pewresearch.org/data-labs/2026/08/20/how-much-of-the-internet-is-written-with-ai/"
        ));
    }

    #[test]
    fn rejects_example_news_site_hallucination() {
        assert!(is_junk_or_search_url(
            "https://www.example-news-site.com/rtk-ai-labs-ceo-update"
        ));
        assert!(is_junk_or_search_url("https://example.com/x"));
        assert!(is_junk_or_search_url("https://foo.example/bar"));
        assert!(!is_junk_or_search_url(
            "https://labs.sogeti.com/the-hidden-cost-of-ai-coding-how-rtk-helps-developers-defeat-the-token-tax-part-2/"
        ));
    }

    #[test]
    fn filter_drops_placeholders_keeps_sogeti() {
        let kept = filter_publisher_urls(&[
            "https://www.example-news-site.com/rtk-ai-labs-ceo-update".into(),
            "https://labs.sogeti.com/token-tax".into(),
        ]);
        assert_eq!(kept, vec!["https://labs.sogeti.com/token-tax".to_string()]);
    }

    #[test]
    fn linkedin_host_detects_com_and_lnkd() {
        assert!(is_linkedin_host("https://www.linkedin.com/in/x"));
        assert!(is_linkedin_host("https://lnkd.in/abc"));
        assert!(!is_linkedin_host("https://labs.sogeti.com/x"));
    }

    #[test]
    fn pulse_url_matrix() {
        assert!(is_linkedin_pulse_url(
            "https://www.linkedin.com/pulse/when-you-start-speaking-emojis-engage-your-consumers-adrien-lepert/"
        ));
        assert!(is_linkedin_pulse_url(
            "https://www.linkedin.com/pulse/foo-bar/?trackingId=abc#frag"
        ));
        assert!(!is_linkedin_pulse_url(
            "https://www.linkedin.com/posts/gregoryroussac_x-activity-123"
        ));
        assert!(!is_linkedin_pulse_url(
            "https://www.linkedin.com/in/someone"
        ));
        assert!(!is_linkedin_pulse_url("https://lnkd.in/abc"));
        assert!(!is_linkedin_pulse_url("https://example.com/pulse/x"));
    }

    #[test]
    fn canonicalize_ingest_strips_query_and_fragment() {
        assert_eq!(
            canonicalize_ingest_url("https://www.linkedin.com/pulse/foo-bar/?trackingId=abc#x"),
            "https://www.linkedin.com/pulse/foo-bar"
        );
        assert_eq!(
            canonicalize_ingest_url("https://labs.sogeti.com/a/"),
            "https://labs.sogeti.com/a"
        );
    }

    #[test]
    fn x_status_url_and_id() {
        assert_eq!(
            x_status_id("https://x.com/Interchouette/status/1234567890").as_deref(),
            Some("1234567890")
        );
        assert_eq!(
            x_status_id("https://twitter.com/foo/status/99?s=20").as_deref(),
            Some("99")
        );
        assert!(is_x_status_url(
            "https://x.com/i/web/status/2088223231035551912"
        ));
        assert_eq!(
            x_status_id("https://x.com/i/web/status/2088223231035551912").as_deref(),
            Some("2088223231035551912")
        );
        assert!(!is_x_status_url("https://x.com/a"));
        assert!(!is_x_status_url("https://labs.sogeti.com/status/1"));
        assert!(is_allowed_tweet_cite(
            "https://x.com/Interchouette/status/1"
        ));
        assert!(is_allowed_tweet_cite("https://labs.sogeti.com/token-tax"));
        assert!(!is_allowed_tweet_cite("https://www.linkedin.com/posts/x"));
        assert!(!is_allowed_tweet_cite("https://www.example.com/a"));
        assert!(!is_allowed_tweet_cite("https://"));
        assert!(extract_https_urls("https://\nPayouts.com has picked").is_empty());
    }

    #[test]
    fn scrub_strips_backtick_and_slash_for_key() {
        assert_eq!(
            scrub_https_url("https://itsfoss.com/news/rust-code-repo-ai-policy`"),
            "https://itsfoss.com/news/rust-code-repo-ai-policy"
        );
        assert!(same_publisher_url(
            "https://itsfoss.com/news/rust-code-repo-ai-policy/",
            "https://itsfoss.com/news/rust-code-repo-ai-policy`"
        ));
    }
}

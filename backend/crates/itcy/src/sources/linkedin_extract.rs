// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Extract public `LinkedIn` activity body from guest HTML (Tor fetch).
//!
//! Policy: no minimum length (no 40 / 80 char gates). Any non-empty usable
//! title, description, commentary, or trimmed page text is enough to enrich.
//! Empty link-only posts and empty-caption reposts are legal: index whatever
//! public text the guest page exposes (often the original under "reposted this").

use crate::sources::html::html_to_text;

/// Result of extracting a public `LinkedIn` post/repost page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedInExtract {
    pub text: String,
    /// True when usable public text was found (not guest-only chrome / unavailable).
    pub ok: bool,
    pub reason: &'static str,
}

fn non_empty(s: &str) -> bool {
    !s.trim().is_empty()
}

/// Markers that appear on guest chrome / soft walls (OK if body also present).
fn looks_like_guest_chrome(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("agree & join")
        || lower.contains("agree and join")
        || lower.contains("sign in to view more")
        || lower.contains("join now") && lower.contains("sign in")
        || lower.contains("authwall")
}

/// True when guest HTML still shows a real activity card (not empty auth chrome).
fn looks_like_public_post_card(html: &str, full_text: &str) -> bool {
    html.contains("SocialMediaPosting")
        || full_text.contains("’s Post")
        || full_text.contains("'s Post")
        || full_text.to_ascii_lowercase().contains("reposted this")
        || extract_meta_content(html, "og:title")
            .is_some_and(|t| non_empty(&collapse(&html_entities_lite(&t))))
}

/// Deleted / removed activity (reachable HTML, nothing to enrich).
fn looks_like_unavailable(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("this post is unavailable")
        || lower.contains("this post isn’t available")
        || lower.contains("this post isn't available")
        || lower.contains("post is unavailable")
}

const fn accept(text: String, reason: &'static str) -> LinkedInExtract {
    LinkedInExtract {
        text,
        ok: true,
        reason,
    }
}

/// Pull commentary from known `LinkedIn` guest markup, else meta / schema.
#[must_use]
pub fn extract_linkedin_public_post(html: &str) -> LinkedInExtract {
    // Deleted posts before guest-chrome heuristics (Agree & Join still appears).
    if looks_like_unavailable(html) {
        let full = collapse(&html_to_text(html));
        return LinkedInExtract {
            text: full.chars().take(400).collect(),
            ok: false,
            reason: "post_unavailable",
        };
    }
    if let Some(body) = extract_commentary_attr(html) {
        let text = collapse(&html_to_text(&body));
        if non_empty(&text) && !looks_like_guest_chrome(&text) {
            return accept(text, "commentary");
        }
    }
    if let Some(body) = extract_between(
        html,
        "data-test-id=\"main-feed-activity-card__commentary\"",
        "</p>",
    ) {
        let text = collapse(&html_to_text(&body));
        if non_empty(&text) && !looks_like_guest_chrome(&text) {
            return accept(text, "commentary_tag");
        }
    }
    // Prefer title then description for link-only cards (any non-empty length).
    if let Some(title) = extract_meta_content(html, "og:title") {
        let text = collapse(&html_entities_lite(&title));
        if non_empty(&text) && !looks_like_guest_chrome(&text) {
            return accept(text, "og_title");
        }
    }
    if let Some(desc) = extract_meta_content(html, "og:description")
        .or_else(|| extract_meta_content(html, "description"))
    {
        let text = collapse(&html_entities_lite(&desc));
        if non_empty(&text) && !looks_like_guest_chrome(&text) {
            return accept(text, "og_description");
        }
    }
    if let Some(schema) = extract_schema_posting_text(html) {
        let text = collapse(&html_entities_lite(&schema));
        if non_empty(&text) && !looks_like_guest_chrome(&text) {
            return accept(text, "schema_text");
        }
    }

    let full = collapse(&html_to_text(html));
    // Repost / post card: trim leading join chrome; keep original under "reposted this".
    if let Some(trimmed) = trim_leading_guest_chrome(&full) {
        if non_empty(&trimmed) {
            return accept(trimmed.chars().take(2000).collect(), "trimmed_full");
        }
    }
    if non_empty(&full) && !looks_like_guest_chrome(&full) {
        return accept(full, "full_html_text");
    }
    // Empty auth chrome with no post card / title: hard wall signal for enrich.
    if looks_like_guest_chrome(&full) && !looks_like_public_post_card(html, &full) {
        return LinkedInExtract {
            text: full.chars().take(400).collect(),
            ok: false,
            reason: "guest_chrome_no_body",
        };
    }
    LinkedInExtract {
        text: full.chars().take(400).collect(),
        ok: false,
        reason: "too_thin",
    }
}

fn extract_commentary_attr(html: &str) -> Option<String> {
    let marker = "data-test-id=\"main-feed-activity-card__commentary\"";
    let start = html.find(marker)?;
    let after = &html[start..];
    let gt = after.find('>')?;
    let inner_start = gt + 1;
    let rest = &after[inner_start..];
    let end = rest.find("</p>")?;
    Some(rest[..end].to_string())
}

fn extract_between(html: &str, start_marker: &str, end_marker: &str) -> Option<String> {
    let start = html.find(start_marker)?;
    let after = &html[start + start_marker.len()..];
    let gt = after.find('>')?;
    let inner = &after[gt + 1..];
    let end = inner.find(end_marker)?;
    Some(inner[..end].to_string())
}

fn extract_meta_content(html: &str, prop: &str) -> Option<String> {
    let patterns = [format!("property=\"{prop}\""), format!("name=\"{prop}\"")];
    for pat in patterns {
        if let Some(idx) = html.find(&pat) {
            let window = &html[idx..html.len().min(idx + 800)];
            if let Some(cidx) = window.find("content=\"") {
                let rest = &window[cidx + "content=\"".len()..];
                let end = rest.find('"')?;
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

fn extract_schema_posting_text(html: &str) -> Option<String> {
    let marker = "\"@type\":\"SocialMediaPosting\"";
    let start = html.find(marker)?;
    let window = &html[start..html.len().min(start + 2500)];
    let key = "\"text\":\"";
    let tidx = window.find(key)?;
    let rest = &window[tidx + key.len()..];
    let end = rest.find('"')?;
    let raw = &rest[..end];
    if raw.is_empty() {
        return None;
    }
    Some(raw.to_string())
}

fn trim_leading_guest_chrome(full: &str) -> Option<String> {
    let markers = [
        "Skip to main content",
        "Gregory Roussac’s Post",
        "Gregory Roussac's Post",
        "reposted this",
    ];
    for m in markers {
        if let Some(i) = full.find(m) {
            let sliced = full[i..].trim();
            if non_empty(sliced) {
                return Some(sliced.to_string());
            }
        }
    }
    None
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn html_entities_lite(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;#39;", "'")
        .replace("&#x27;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_commentary_not_join_chrome() {
        let html = r#"
        <html><body>
        <p>Agree & Join LinkedIn Sign in Join now</p>
        <p class="x" data-test-id="main-feed-activity-card__commentary">
        Termuner is a Rust webradio tuner for the terminal with SQLite memory and a .deb package.
        </p>
        </body></html>
        "#;
        let got = extract_linkedin_public_post(html);
        assert!(got.ok, "{got:?}");
        assert!(got.text.contains("Termuner"));
        assert!(!got.text.contains("Agree & Join"));
    }

    #[test]
    fn short_title_one_char_ok() {
        let html = r#"
        <html><head>
        <meta property="og:title" content="X">
        </head><body>
        <h1>Gregory Roussac's Post</h1>
        </body></html>
        "#;
        let got = extract_linkedin_public_post(html);
        assert!(got.ok, "{got:?}");
        assert_eq!(got.reason, "og_title");
        assert_eq!(got.text, "X");
    }

    #[test]
    fn guest_chrome_only_fails() {
        let html = r"
        <html><body>
        <h1>Agree & Join LinkedIn</h1>
        <p>Sign in to view more content. Create your free account or sign in.</p>
        </body></html>
        ";
        let got = extract_linkedin_public_post(html);
        assert!(!got.ok, "{got:?}");
        assert_eq!(got.reason, "guest_chrome_no_body");
    }

    #[test]
    fn unavailable_beats_guest_chrome() {
        let html = r"
        <html><body>
        <h1>Agree & Join LinkedIn</h1>
        <p>This post is unavailable.</p>
        <p>Sign in to view more content.</p>
        </body></html>
        ";
        let got = extract_linkedin_public_post(html);
        assert!(!got.ok, "{got:?}");
        assert_eq!(got.reason, "post_unavailable");
    }

    #[test]
    fn repost_trimmed_includes_original_body() {
        let html = r"
        <html><body>
        <h1>Agree & Join LinkedIn</h1>
        <p>Gregory Roussac reposted this</p>
        <p>MetaMask is a crypto wallet and gateway to blockchain apps.
        You can store ether and other digital assets, then use them across the web.</p>
        </body></html>
        ";
        let got = extract_linkedin_public_post(html);
        assert!(got.ok, "{got:?}");
        assert_eq!(got.reason, "trimmed_full");
        assert!(
            got.text.to_ascii_lowercase().contains("metamask"),
            "{got:?}"
        );
    }

    #[test]
    fn link_only_og_title_ok_despite_agree_join() {
        let html = r#"
        <html><head>
        <meta property="og:title" content="Why developer expertise matters more than ever in the age of AI | Gregory Roussac">
        <meta property="og:description" content="Why developer expertise matters more than ever in the age of AI">
        </head><body>
        <h1>Agree & Join LinkedIn</h1>
        <h1>Gregory Roussac's Post</h1>
        <p>Why developer expertise matters more than ever in the age of AI</p>
        </body></html>
        "#;
        let got = extract_linkedin_public_post(html);
        assert!(got.ok, "{got:?}");
        assert_eq!(got.reason, "og_title");
        assert!(got.text.contains("developer expertise"));
    }

    #[test]
    fn schema_text_ok_for_link_only() {
        let html = r#"
        <html><head>
        <script type="application/ld+json">
        {"@type":"SocialMediaPosting","text":"Why developer expertise matters more than ever in the age of AI"}
        </script>
        </head><body>
        <p>Agree & Join LinkedIn Sign in</p>
        </body></html>
        "#;
        let got = extract_linkedin_public_post(html);
        assert!(got.ok, "{got:?}");
        assert_eq!(got.reason, "schema_text");
    }

    #[test]
    fn real_cached_9060_link_only_is_ok() {
        let path = "/tmp/li-fixtures/9060.html";
        let Ok(html) = std::fs::read_to_string(path) else {
            return;
        };
        let got = extract_linkedin_public_post(&html);
        assert!(got.ok, "{got:?}");
        assert!(got.text.contains("developer expertise"), "{got:?}");
    }

    #[test]
    fn real_cached_8987_is_unavailable() {
        let path = "/tmp/li-fixtures/8987.html";
        let Ok(html) = std::fs::read_to_string(path) else {
            return;
        };
        let got = extract_linkedin_public_post(&html);
        assert!(!got.ok, "{got:?}");
        assert_eq!(got.reason, "post_unavailable");
    }

    #[test]
    fn real_cached_9058_link_only_is_ok() {
        let path = "/tmp/li-fixtures/9058.html";
        let Ok(html) = std::fs::read_to_string(path) else {
            return;
        };
        let got = extract_linkedin_public_post(&html);
        assert!(got.ok, "{got:?}");
        assert!(
            got.text.contains("AI-generated") || got.text.contains("vulnerable"),
            "{got:?}"
        );
    }
}

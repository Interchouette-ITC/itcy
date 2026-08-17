// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Cheap HTML → plain text for public URL ingest.

/// Prefer Medium-style Apollo paragraph JSON, then `<article>` / `<main>`, else full strip.
///
/// Medium 2026 often ships both `window.__APOLLO_STATE__` (full body) and a thin
/// SSR shell / TOC in the DOM. Crawler HTML is not "impossible SSR"; it is hybrid.
/// Prefer Apollo paragraphs when present so we do not confuse TOC chrome with the article.
#[must_use]
pub fn extract_page_text(html: &str) -> String {
    if let Some(apollo) = extract_apollo_paragraphs(html) {
        if apollo.chars().count() >= 120 {
            return apollo;
        }
    }
    let lower = html.to_ascii_lowercase();
    for tag in ["article", "main"] {
        if let Some(slice) = first_element_inner(html, &lower, tag) {
            let text = html_to_text(slice);
            if text.chars().count() >= 120 {
                return text;
            }
        }
    }
    html_to_text(html)
}

/// Pulls `Paragraph:*` `.text` fields from `window.__APOLLO_STATE__ = {...};`.
fn extract_apollo_paragraphs(html: &str) -> Option<String> {
    const MARKER: &str = "window.__APOLLO_STATE__ = ";
    let start = html.find(MARKER)? + MARKER.len();
    let rest = &html[start..];
    let end = rest.find(";</script>")?;
    let json = rest[..end].trim();
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = value.as_object()?;
    let mut keyed: Vec<(u32, String)> = Vec::new();
    for (k, v) in obj {
        if !k.starts_with("Paragraph:") {
            continue;
        }
        let Some(text) = v.get("text").and_then(|t| t.as_str()) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        let idx = k
            .rsplit('_')
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        keyed.push((idx, text.to_string()));
    }
    if keyed.is_empty() {
        return None;
    }
    keyed.sort_by_key(|(i, _)| *i);
    Some(
        keyed
            .into_iter()
            .map(|(_, t)| t)
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

fn first_element_inner<'a>(html: &'a str, lower: &str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let start_tag = lower.find(&open)?;
    let after_lt = start_tag + open.len();
    let gt = lower[after_lt..].find('>')? + after_lt;
    let body_start = gt + 1;
    let end = lower[body_start..].find(&close)? + body_start;
    Some(&html[body_start..end])
}

/// Strips tags and collapses whitespace. Not a full readability engine.
///
/// Walks by Unicode scalar values so multi-byte UTF-8 (e.g. `’`) never panics.
#[must_use]
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let lower = html.to_ascii_lowercase();
    for (i, c) in html.char_indices() {
        let rest = &lower[i..];
        if !in_tag && rest.starts_with("<script") {
            in_script = true;
        }
        if !in_tag && rest.starts_with("<style") {
            in_style = true;
        }
        if in_script && rest.starts_with("</script") {
            in_script = false;
        }
        if in_style && rest.starts_with("</style") {
            in_style = false;
        }

        if c == '<' {
            in_tag = true;
            continue;
        }
        if c == '>' {
            in_tag = false;
            out.push(' ');
            continue;
        }
        if !in_tag && !in_script && !in_style {
            out.push(c);
        }
    }
    collapse_ws(&html_entities_basic(&out))
}

fn html_entities_basic(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&rsquo;", "'")
        .replace("&lsquo;", "'")
        .replace("&rdquo;", "\"")
        .replace("&ldquo;", "\"")
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Short operator-facing preview of ingested / enriched body text.
#[must_use]
pub fn content_preview(text: &str, max_chars: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max_chars {
        return t.to_string();
    }
    let mut out: String = t.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

const ARTICLE_BLURB_MAX: usize = 420;

/// Short article blurb for digest Slack: Open Graph / meta description, else first sentences.
#[must_use]
pub fn article_blurb(html: &str) -> Option<String> {
    for (attr, key) in [
        ("property", "og:description"),
        ("property", "twitter:description"),
        ("name", "twitter:description"),
        ("name", "description"),
    ] {
        if let Some(raw) = meta_named(html, attr, key) {
            if let Some(b) = usable_blurb(&raw) {
                return Some(b);
            }
        }
    }
    usable_blurb(&extract_page_text(html))
}

fn usable_blurb(raw: &str) -> Option<String> {
    let t = collapse_ws(&html_entities_basic(raw));
    if !is_usable_blurb(&t) {
        return None;
    }
    Some(first_sentences(&t, ARTICLE_BLURB_MAX))
}

/// True when `s` is long enough and not nav / cookie chrome.
#[must_use]
pub fn looks_like_article_blurb(s: &str) -> bool {
    is_usable_blurb(s)
}

fn is_usable_blurb(s: &str) -> bool {
    let t = s.trim();
    if t.chars().count() < 60 {
        return false;
    }
    let l = t.to_ascii_lowercase();
    if l.contains("aria-label") {
        return false;
    }
    let junk = [
        "view more",
        "go to content",
        "skip to",
        "unspecific search",
        "generic or unspecific",
        "cookie",
        "enable javascript",
    ];
    if junk.iter().any(|j| l.contains(j)) {
        return false;
    }
    true
}

fn first_sentences(text: &str, max_chars: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max_chars {
        return t.to_string();
    }
    let mut out = String::new();
    for sent in t.split_inclusive(['.', '!', '?']) {
        let candidate = format!("{out}{sent}");
        if candidate.chars().count() > max_chars && !out.is_empty() {
            break;
        }
        out = candidate;
        if out.chars().count() >= max_chars.saturating_sub(40) {
            break;
        }
    }
    if out.is_empty() {
        return content_preview(t, max_chars);
    }
    out.trim().to_string()
}

fn meta_named(html: &str, attr: &str, value: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let needles = [format!("{attr}=\"{value}\""), format!("{attr}='{value}'")];
    for needle in needles {
        let mut search = 0;
        while let Some(rel) = lower.get(search..).and_then(|s| s.find(&needle)) {
            let pos = search + rel;
            let start = html.get(..pos).and_then(|h| h.rfind('<'))?;
            let rel_end = html.get(start..).and_then(|h| h.find('>'))?;
            let tag = html.get(start..=start + rel_end)?;
            if let Some(c) = content_attr(tag) {
                let t = c.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
            search = pos + needle.len();
        }
    }
    None
}

fn content_attr(tag: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let pos = lower.find("content=")?;
    let after = tag.get(pos + 8..)?.trim_start();
    let q = after.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let inner = after.get(q.len_utf8()..)?;
    let end = inner.find(q)?;
    Some(inner[..end].to_string())
}

/// Infers a short subject tag from the page title (prefer title over body noise).
/// Keeps hyphenated compounds (`open-weight`) and up to six content tokens.
#[must_use]
pub fn infer_subject(title: &str, body: &str) -> String {
    let stop = [
        "the",
        "a",
        "an",
        "and",
        "or",
        "of",
        "to",
        "in",
        "for",
        "on",
        "with",
        "is",
        "as",
        "by",
        "from",
        "at",
        "into",
        "over",
        "after",
        "before",
        "about",
        "via",
        "vs",
        "infoworld",
        "news",
        "article",
        "topics",
        "latest",
        "menu",
        "search",
        "close",
        "medium",
        "sign",
        "post",
        "repost",
    ];
    let mut words = content_tokens(title, &stop);
    if words.len() < 5 {
        for w in content_tokens(body, &stop) {
            if !words.iter().any(|x| x == &w) {
                words.push(w);
            }
            if words.len() >= 6 {
                break;
            }
        }
    }
    words.truncate(6);
    if words.is_empty() {
        return "general".into();
    }
    words.join(" ")
}

fn content_tokens(text: &str, stop: &[&str]) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>, stop: &[&str]| {
        if cur.is_empty() {
            return;
        }
        let w = std::mem::take(cur);
        if w.len() > 2 && !stop.contains(&w.as_str()) {
            out.push(w);
        }
    };
    for c in lower.chars() {
        if c.is_alphanumeric() || c == '-' {
            cur.push(c);
        } else {
            flush(&mut cur, &mut out, stop);
        }
    }
    flush(&mut cur, &mut out, stop);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_script_and_tags() {
        let html = r"<html><script>evil()</script><p>Hello <b>Rust</b></p></html>";
        let text = html_to_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("Rust"));
        assert!(!text.contains("evil"));
    }

    #[test]
    fn utf8_curly_apostrophe_does_not_panic() {
        let html = "<p>It\u{2019}s fine - LinkedIn copy</p>";
        let text = html_to_text(html);
        assert!(text.contains("fine"));
        assert!(text.contains("LinkedIn"));
    }

    #[test]
    fn infer_subject_skips_stops() {
        let s = infer_subject("The Rust Async Guide", "learn tokio runtime");
        assert!(s.contains("rust"));
    }

    #[test]
    fn infer_subject_keeps_open_weight_from_title() {
        let title = "Anthropic rejects open-weight AI bans, calls for China chip controls and safety tests | InfoWorld";
        let s = infer_subject(title, "Topics Latest Menu Search Close Analytics");
        assert_eq!(
            s, "anthropic rejects open-weight bans calls china",
            "got subject={s}"
        );
    }

    #[test]
    fn extract_page_prefers_article_over_chrome() {
        let html = r"<html><body>
<nav>Sign in Topics Latest Menu Search Close Analytics</nav>
<article><p>Angular signals drive form state without event pipelines. Signal forms update the model. More body text to clear the length floor for article preference.</p></article>
</body></html>";
        let text = extract_page_text(html);
        assert!(text.to_ascii_lowercase().contains("angular signals"));
        assert!(!text.to_ascii_lowercase().contains("sign in topics"));
    }

    #[test]
    fn extract_page_prefers_apollo_paragraphs() {
        let html = r#"<html><body>
<article><p>Short TOC only</p></article>
<script>window.__APOLLO_STATE__ = {"Paragraph:x_0":{"__typename":"Paragraph","text":"Angular signals are the preferred path for local UI state when forms and derived values stay in the same reactive graph."},"Paragraph:x_1":{"__typename":"Paragraph","text":"RxJS remains for streams and websockets across the rest of the application boundary."}};</script>
</body></html>"#;
        let text = extract_page_text(html);
        assert!(text.contains("preferred path"));
        assert!(text.contains("RxJS remains"));
        assert!(!text.contains("Short TOC"));
    }

    #[test]
    fn article_blurb_prefers_og_description() {
        let html = r#"<html><head>
<meta property="og:description" content="Signals keep form state in one graph so templates stay declarative without event pipelines.">
</head><body><nav>Search View more Go to content</nav></html>"#;
        let b = article_blurb(html).expect("og blurb");
        assert!(b.contains("Signals keep form state"));
        assert!(!b.to_ascii_lowercase().contains("view more"));
    }

    #[test]
    fn article_blurb_rejects_nav_chrome() {
        let html = r"<html><body><nav>Search View more Go to content Generic or unspecific search aria-label skip</nav></body></html>";
        assert!(article_blurb(html).is_none());
    }
}

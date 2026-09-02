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
    if let Some(text) = extract_articleish_text(html) {
        return text;
    }
    html_to_text(html)
}

/// Article-ish body only (Apollo paragraphs, JSON-LD, `<article>` / `<main>`, WP content).
///
/// Returns `None` when the page is chrome / SPA shell with no real article region.
/// Publisher cite probes use this so a fat Next.js 200 shell does not pass as a cite.
#[must_use]
pub fn extract_articleish_text(html: &str) -> Option<String> {
    if let Some(apollo) = extract_apollo_paragraphs(html) {
        if apollo.chars().count() >= 120 {
            return Some(apollo);
        }
    }
    if let Some(ld) = extract_json_ld_article_text(html) {
        if ld.chars().count() >= 120 {
            return Some(ld);
        }
    }
    let lower = html.to_ascii_lowercase();
    for tag in ["article", "main"] {
        for slice in all_element_inners(html, &lower, tag) {
            let text = html_to_text(slice);
            if text.chars().count() >= 120 {
                return Some(text);
            }
        }
    }
    for class in [
        "entry-content",
        "post-content",
        "theme-post-content",
        "blog-post-content",
        "article-body",
    ] {
        if let Some(slice) = element_inner_by_class_token(html, &lower, class) {
            let text = html_to_text(slice);
            if text.chars().count() >= 120 {
                return Some(text);
            }
        }
    }
    // SPA shells (Angular/React) often ship only social cards in the first HTML.
    // Title + meta/og description is enough to prove a real publisher page exists;
    // fat chrome with no description still returns None.
    if let Some(card) = extract_social_card_article_text(html) {
        if card.chars().count() >= 120 {
            return Some(card);
        }
    }
    if page_marked_as_article(&lower) {
        if let Some(blurb) = article_blurb(html) {
            if blurb.chars().count() >= 120 {
                return Some(blurb);
            }
        }
    }
    None
}

/// Title + meta/og/twitter description from a JS shell that has no `<article>` yet.
fn extract_social_card_article_text(html: &str) -> Option<String> {
    // Do not call `article_blurb` here: it falls back to `extract_page_text` and would recurse.
    let desc = meta_description_only(html).and_then(|raw| usable_blurb(&raw))?;
    let title = extract_document_title(html)
        .map(|t| collapse_ws(&html_entities_basic(&t)))
        .filter(|t| t.chars().count() >= 12);
    let mut parts = Vec::new();
    if let Some(t) = title {
        let low = t.to_ascii_lowercase();
        if !low.contains("404") && !low.contains("not found") && low != "home" {
            parts.push(t);
        }
    }
    parts.push(desc);
    let joined = parts.join("\n\n");
    (joined.chars().count() >= 120).then_some(joined)
}

fn meta_description_only(html: &str) -> Option<String> {
    for (attr, key) in [
        ("property", "og:description"),
        ("property", "twitter:description"),
        ("name", "twitter:description"),
        ("name", "description"),
    ] {
        if let Some(raw) = meta_named(html, attr, key) {
            let t = raw.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

fn extract_document_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let after = &lower[start..];
    let gt = after.find('>')? + 1;
    let rest = &html[start + gt..];
    let end = rest.to_ascii_lowercase().find("</title>")?;
    let t = rest[..end].trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn page_marked_as_article(lower: &str) -> bool {
    lower.contains("property=\"og:type\" content=\"article\"")
        || lower.contains("property='og:type' content='article'")
}

fn all_element_inners<'a>(html: &'a str, lower: &str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(rel) = lower[search..].find(&open) {
        let start_tag = search + rel;
        let after_lt = start_tag + open.len();
        if let Some(gt_rel) = lower[after_lt..].find('>') {
            let gt = after_lt + gt_rel;
            let body_start = gt + 1;
            if let Some(end_rel) = lower[body_start..].find(&close) {
                let end = body_start + end_rel;
                out.push(&html[body_start..end]);
                search = end + close.len();
                continue;
            }
        }
        search = start_tag + 1;
    }
    out
}

fn element_inner_by_class_token<'a>(html: &'a str, lower: &str, class: &str) -> Option<&'a str> {
    let mut search = 0;
    while let Some(rel) = lower[search..].find(class) {
        let pos = search + rel;
        if !is_html_class_token_at(lower, pos, class) {
            search = pos + class.len();
            continue;
        }
        let open = html.get(..pos)?.rfind('<')?;
        if let Some(inner) = element_inner_from_open_tag(html, lower, open) {
            return Some(inner);
        }
        search = pos + class.len();
    }
    None
}

fn is_html_class_token_at(lower: &str, pos: usize, token: &str) -> bool {
    let before = lower.get(pos.saturating_sub(1)..pos).unwrap_or("");
    let after = lower
        .get(pos + token.len()..=pos + token.len())
        .unwrap_or("");
    let ok_before = pos == 0
        || before
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || c == '"' || c == '\'');
    let ok_after = after.is_empty()
        || after
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || c == '"' || c == '\'');
    ok_before && ok_after
}

fn element_inner_from_open_tag<'a>(html: &'a str, lower: &str, open: usize) -> Option<&'a str> {
    let after_open = &lower[open..];
    let tag_end = after_open.find('>')? + open;
    let tag_head = &lower[open + 1..tag_end];
    let tag = tag_head.split_whitespace().next()?;
    if tag.starts_with('/') {
        return None;
    }
    let close = format!("</{tag}>");
    let body_start = tag_end + 1;
    let end = lower[body_start..].find(&close)? + body_start;
    Some(&html[body_start..end])
}

fn extract_json_ld_article_text(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let marker = "application/ld+json";
    let mut search = 0;
    let mut best: Option<String> = None;
    while let Some(rel) = lower[search..].find(marker) {
        let pos = search + rel;
        let Some(script_start) = html.get(..pos).and_then(|h| h.rfind("<script")) else {
            search = pos + marker.len();
            continue;
        };
        let Some(gt_rel) = html.get(pos..).and_then(|h| h.find('>')) else {
            search = pos + marker.len();
            continue;
        };
        let gt = pos + gt_rel + 1;
        let Some(end_rel) = html.get(gt..).and_then(|h| h.find("</script>")) else {
            search = pos + marker.len();
            continue;
        };
        let json_str = html.get(gt..gt + end_rel)?.trim();
        if let Some(text) = json_ld_article_plain(json_str) {
            let n = text.chars().count();
            if best.as_ref().is_none_or(|b| b.chars().count() < n) {
                best = Some(text);
            }
        }
        search = gt + end_rel + 9;
        let _ = script_start;
    }
    best.filter(|t| t.chars().count() >= 120)
}

fn json_ld_article_plain(json_str: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let mut parts = Vec::new();
    collect_json_ld_article_parts(&v, &mut parts);
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n\n"))
}

fn collect_json_ld_article_parts(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(map) => {
            if json_ld_type_is(map.get("@type"), "Article")
                || json_ld_type_is(map.get("@type"), "BlogPosting")
                || json_ld_type_is(map.get("@type"), "NewsArticle")
            {
                push_json_ld_field(map, "headline", out);
                push_json_ld_field(map, "description", out);
                push_json_ld_field(map, "articleBody", out);
            } else if json_ld_type_is(map.get("@type"), "WebPage") {
                push_json_ld_field(map, "name", out);
                push_json_ld_field(map, "description", out);
            }
            for val in map.values() {
                collect_json_ld_article_parts(val, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                collect_json_ld_article_parts(item, out);
            }
        }
        _ => {}
    }
}

fn json_ld_type_is(v: Option<&serde_json::Value>, want: &str) -> bool {
    match v {
        Some(serde_json::Value::String(s)) => s == want,
        Some(serde_json::Value::Array(arr)) => {
            arr.iter().filter_map(|x| x.as_str()).any(|s| s == want)
        }
        _ => false,
    }
}

fn push_json_ld_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    out: &mut Vec<String>,
) {
    let Some(raw) = map.get(key).and_then(|v| v.as_str()) else {
        return;
    };
    let t = html_entities_basic(raw.trim());
    if t.chars().count() >= 40 && !out.iter().any(|x| x == &t) {
        out.push(t);
    }
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

/// True when HTML is a Cloudflare/Turnstile interstitial, not publisher content.
#[must_use]
pub fn looks_like_cloudflare_challenge(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("challenges.cloudflare.com")
        || lower.contains("cdn-cgi/challenge-platform")
        || lower.contains("just a moment")
        || lower.contains("un instant")
        || lower.contains("performing security verification")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cloudflare_challenge_shell() {
        let html = r#"<html><head><title>Just a moment...</title></head>
<body><script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script></body></html>"#;
        assert!(looks_like_cloudflare_challenge(html));
    }

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

    #[test]
    fn extract_json_ld_article_from_scylladb_style_page() {
        use crate::sources::publisher_url::evaluate_publisher_probe;
        let html = r#"<html><head>
<meta property="og:type" content="article" />
<script type="application/ld+json">{"@context":"https://schema.org","@type":"Article","headline":"Building a New Rust Driver for ScyllaDB DynamoDB API with 58% More Throughput","description":"How our new Rust driver load-balances DynamoDB-style requests across a ScyllaDB cluster, and how we extended Latte to measure its performance on Alternator workloads with cluster-aware routing."}</script>
</head><body>
<article><div>{{{ item.image }}}</div></article>
</body></html>"#;
        let text = extract_articleish_text(html).expect("json-ld article");
        assert!(text.contains("58%"));
        assert!(text.contains("Rust driver"));
        evaluate_publisher_probe(200, html).expect("probe accepts json-ld article page");
    }

    #[test]
    fn extract_json_ld_webpage_from_futurum_style_page() {
        use crate::sources::publisher_url::evaluate_publisher_probe;
        let html = r#"<html><head>
<meta property="og:type" content="article" />
<script type="application/ld+json">{"@context":"https://schema.org","@graph":[{"@type":"WebPage","name":"ScyllaDB Rust Driver Boosts DynamoDB Throughput 58%","description":"ScyllaDB released an open-source Rust driver for its DynamoDB-compatible Alternator API, achieving 58% higher throughput than AWS SDK on 3-node clusters while maintaining full API compatibility and cluster-aware load balancing."}]}</script>
</head><body><div class="elementor-widget-theme-post-content">Publication Date</div></body></html>"#;
        let text = extract_articleish_text(html).expect("json-ld webpage");
        assert!(text.contains("58%"));
        evaluate_publisher_probe(200, html).expect("probe accepts futurum-style webpage");
    }

    #[test]
    fn scylladb_live_html_fixture_has_articleish_body() {
        let html = std::fs::read_to_string("/tmp/scylla-probe.html").unwrap_or_default();
        if html.is_empty() {
            return;
        }
        let text = extract_articleish_text(&html).expect("live scylladb fixture");
        assert!(text.chars().count() >= 120);
        assert!(text.to_ascii_lowercase().contains("rust"));
    }

    #[test]
    fn google_bughunters_spa_shell_is_articleish_via_social_cards() {
        // DRAFT-20260831-000134: probe rejected this as "no article body" while the
        // page is a real Google Bug Hunters post (Angular <app-root> + meta only).
        use crate::sources::publisher_url::evaluate_publisher_probe;
        let html = r#"<!doctype html>
<html lang="en-US">
  <head>
    <title>Blog: Scaling Memory Safety: AI-Assisted Rewrites of C/C++ Dependencies to Rust</title>
    <meta name="description" content="This blog post describes how we used AI to help us rewrite a C library (giflib) to Rust to mitigate memory safety vulnerabilities." />
    <meta property="og:description" content="This blog post describes how we used AI to help us rewrite a C library (giflib) to Rust to mitigate memory safety vulnerabilities." />
    <meta property="og:url" content="https://bughunters.google.com/blog/scaling-memory-safety" />
  </head>
  <body class="mat-app-background">
    <app-root></app-root>
  </body>
</html>"#;
        let text =
            extract_articleish_text(html).expect("SPA social cards must count as articleish");
        assert!(text.to_ascii_lowercase().contains("giflib"), "{text}");
        assert!(text.to_ascii_lowercase().contains("rust"), "{text}");
        assert!(text.chars().count() >= 120, "len={}", text.chars().count());
        evaluate_publisher_probe(200, html).expect("cite probe must accept bughunters SPA shell");
    }

    #[test]
    fn spa_shell_without_meta_description_still_rejected() {
        use crate::sources::publisher_url::evaluate_publisher_probe;
        let html = r"<!doctype html>
<html><head><title>App</title></head>
<body><app-root></app-root></body></html>";
        assert!(extract_articleish_text(html).is_none());
        let err = evaluate_publisher_probe(200, html).expect_err("empty SPA");
        assert!(err.contains("no article") || err.contains("thin"), "{err}");
    }
}

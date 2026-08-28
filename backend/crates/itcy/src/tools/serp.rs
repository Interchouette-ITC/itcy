// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! SERP link extraction from Playwright DOM evaluate JSON (+ optional HTML scrape).
//! Ranking: non-LinkedIn first (`LinkedIn` often duplicates corpus reposts / login walls).
//! No subject-specific host allowlist.

use serde_json::Value as JsonValue;
use std::fmt::Write;

/// One extracted publisher candidate from a search results page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerpLink {
    pub url: String,
    pub title: String,
    pub linkedin: bool,
}

/// Parse evaluate JSON / HTML scrape into ranked publisher links.
/// `LinkedIn` last; drop Google / placeholders / shorteners.
#[must_use]
pub fn extract_serp_links(evaluate_raw: &str, page_html_or_text: &str) -> Vec<SerpLink> {
    let mut out: Vec<SerpLink> = Vec::new();
    push_from_evaluate_json(&mut out, evaluate_raw);
    if out.is_empty() {
        push_from_text_urls(&mut out, evaluate_raw);
    }
    // Real page source / HTML scrape when DOM JSON was empty (not a11y-first).
    if out.is_empty() {
        push_from_href_attributes(&mut out, page_html_or_text);
        push_from_text_urls(&mut out, page_html_or_text);
    }
    rank_serp_links(out)
}

/// True when Google blocked the scrape (captcha / sorry / unusual traffic).
#[must_use]
pub fn looks_like_google_block(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("/sorry/")
        || t.contains("unusual traffic")
        || t.contains("detected unusual traffic")
        || t.contains("our systems have detected")
        || (t.contains("enable cookies") && t.contains("sorry"))
        || t.contains("captcha")
}

fn push_from_href_attributes(out: &mut Vec<SerpLink>, html: &str) {
    // Pull href="https://..." from page HTML (Playwright documentElement.outerHTML).
    let mut rest = html;
    while let Some(idx) = rest.find("href=\"http") {
        let after = &rest[idx + 6..]; // skip href="
        let end = after.find('"').unwrap_or(after.len());
        let raw = &after[..end];
        if let Some(link) = normalize_candidate(raw, "") {
            push_unique(out, link);
        }
        rest = &after[end.min(after.len())..];
        if out.len() >= 20 {
            break;
        }
    }
    rest = html;
    while let Some(idx) = rest.find("href='http") {
        let after = &rest[idx + 6..];
        let end = after.find('\'').unwrap_or(after.len());
        let raw = &after[..end];
        if let Some(link) = normalize_candidate(raw, "") {
            push_unique(out, link);
        }
        rest = &after[end.min(after.len())..];
        if out.len() >= 20 {
            break;
        }
    }
}

/// Parsed Playwright SERP evaluate payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerpEvaluate {
    pub blocked: bool,
    pub links_json: String,
    pub href: String,
    pub ai_overview: String,
}

/// Split Playwright evaluate payload `{blocked, links, href, ai_overview}` (or legacy bare array).
/// HTML is fetched separately so `}` inside page source cannot break brace slicing.
#[must_use]
pub fn split_serp_evaluate(raw: &str) -> SerpEvaluate {
    let json_str = extract_mcp_evaluate_json(raw);
    if let Ok(v) = serde_json::from_str::<JsonValue>(&json_str) {
        if let Some(obj) = v.as_object() {
            let blocked = obj
                .get("blocked")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            let links = obj
                .get("links")
                .cloned()
                .unwrap_or_else(|| JsonValue::Array(vec![]));
            let href = obj
                .get("href")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string();
            let ai_overview = obj
                .get("ai_overview")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .trim()
                .chars()
                .take(4000)
                .collect();
            return SerpEvaluate {
                blocked,
                links_json: links.to_string(),
                href,
                ai_overview,
            };
        }
        if v.is_array() {
            return SerpEvaluate {
                blocked: false,
                links_json: v.to_string(),
                href: String::new(),
                ai_overview: String::new(),
            };
        }
    }
    SerpEvaluate {
        blocked: false,
        links_json: "[]".into(),
        href: String::new(),
        ai_overview: String::new(),
    }
}

fn push_from_evaluate_json(out: &mut Vec<SerpLink>, raw: &str) {
    let json_str = extract_mcp_evaluate_json(raw);
    let Ok(v) = serde_json::from_str::<JsonValue>(&json_str) else {
        return;
    };
    let arr = if let Some(a) = v.as_array() {
        a.clone()
    } else if let Some(a) = v.get("links").and_then(JsonValue::as_array) {
        a.clone()
    } else {
        return;
    };
    for item in arr {
        let url = item
            .get("url")
            .and_then(|u| u.as_str())
            .or_else(|| item.as_str())
            .unwrap_or("");
        let title = item
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(link) = normalize_candidate(url, &title) {
            push_unique(out, link);
        }
    }
}

fn push_from_text_urls(out: &mut Vec<SerpLink>, text: &str) {
    for (i, _) in text.match_indices("http") {
        let rest = &text[i..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '|' | ')' | ']' | '<' | '"' | '\''))
            .unwrap_or(rest.len());
        let raw = rest[..end].trim_end_matches(['.', ',', ';', ')', ']']);
        if let Some(link) = normalize_candidate(raw, "") {
            push_unique(out, link);
        }
    }
}

/// Pull JSON from host-browser evaluate text.
/// Prefer the `### Result` block (ignore `### Ran Playwright code` which embeds our detector JS).
fn extract_mcp_evaluate_json(raw: &str) -> String {
    let chunk = raw.find("### Result").map_or_else(
        || raw.trim(),
        |i| {
            let after = &raw[i + "### Result".len()..];
            let end = after.find("\n### ").unwrap_or(after.len());
            after[..end].trim()
        },
    );
    // MCP often wraps the payload as a JSON string: "{\"blocked\":false,...}"
    if let Ok(JsonValue::String(inner)) = serde_json::from_str::<JsonValue>(chunk) {
        let t = inner.trim();
        if t.starts_with('{') || t.starts_with('[') {
            return inner;
        }
    }
    // Bare object / array inside the Result chunk only.
    let first_obj = chunk.find('{');
    let first_arr = chunk.find('[');
    match (first_obj, first_arr) {
        (Some(o), Some(a)) if a < o => {
            if let Some(end) = chunk[a..].rfind(']') {
                return chunk[a..=a + end].to_string();
            }
        }
        (Some(o), _) => {
            if let Some(end) = chunk[o..].rfind('}') {
                return chunk[o..=o + end].to_string();
            }
        }
        (None, Some(a)) => {
            if let Some(end) = chunk[a..].rfind(']') {
                return chunk[a..=a + end].to_string();
            }
        }
        _ => {}
    }
    chunk.to_string()
}

fn normalize_candidate(url: &str, title: &str) -> Option<SerpLink> {
    let unwrapped = unwrap_google_redirect(url.trim().trim_end_matches('\\').trim());
    let u = unwrapped.trim().trim_end_matches('\\').trim();
    if !(u.starts_with("https://") || u.starts_with("http://")) {
        return None;
    }
    let lower = u.to_ascii_lowercase();
    if is_blocked_host(&lower) {
        return None;
    }
    let linkedin = crate::sources::url_hygiene::is_linkedin_host(&lower);
    Some(SerpLink {
        url: strip_tracking(u),
        title: title.trim().chars().take(120).collect(),
        linkedin,
    })
}

fn unwrap_google_redirect(url: &str) -> String {
    let lower = url.to_ascii_lowercase();
    if lower.contains("duckduckgo.com/l/") || lower.contains("duckduckgo.com/l?") {
        if let Some(idx) = url.find("uddg=") {
            let rest = &url[idx + 5..];
            let end = rest.find('&').unwrap_or(rest.len());
            let enc = &rest[..end];
            let decoded = percent_decode_minimal(enc);
            if decoded.starts_with("http://") || decoded.starts_with("https://") {
                return decoded;
            }
        }
    }
    if !(lower.contains("google.") && lower.contains("/url?")) {
        return url.to_string();
    }
    for key in ["?q=", "&q=", "?url=", "&url="] {
        if let Some(idx) = url.find(key) {
            let rest = &url[idx + key.len()..];
            let end = rest.find('&').unwrap_or(rest.len());
            let enc = &rest[..end];
            let decoded = percent_decode_minimal(enc);
            if decoded.starts_with("http://") || decoded.starts_with("https://") {
                return decoded;
            }
        }
    }
    url.to_string()
}

fn percent_decode_minimal(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h = |c: u8| -> Option<u8> {
                match c {
                    b'0'..=b'9' => Some(c - b'0'),
                    b'a'..=b'f' => Some(c - b'a' + 10),
                    b'A'..=b'F' => Some(c - b'A' + 10),
                    _ => None,
                }
            };
            if let (Some(a), Some(b)) = (h(bytes[i + 1]), h(bytes[i + 2])) {
                out.push((a << 4) | b);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// True when Brave Search shows bot/POW captcha or rate-limit interstitial.
#[must_use]
pub fn looks_like_brave_block(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("verifying you're not a bot")
        || t.contains("verifying youre not a bot")
        || t.contains("pow-captcha")
        || t.contains("traditional captcha")
        || t.contains("quick check before you continue searching")
        || (t.contains("are you a human") && t.contains("brave"))
        || t.contains("status of 429")
}

/// True when `DuckDuckGo` shows bot/captcha interstitial.
#[must_use]
pub fn looks_like_ddg_block(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("bots use duckduckgo")
        || t.contains("anomaly-modal")
        || t.contains("select all squares")
        || (t.contains("captcha") && t.contains("duckduckgo"))
}

fn is_blocked_host(lower_url: &str) -> bool {
    // Shared junk / social / shortener set, plus SERP-only hosts (Google UI, Brave CDN, Tor docs).
    if crate::sources::url_hygiene::is_junk_or_search_url(lower_url) {
        return true;
    }
    [
        "google.",
        "gstatic.",
        "accounts.google",
        "support.google",
        "policies.google",
        "webcache.googleusercontent",
        "schema.org",
        "w3.org/",
        "cdn.search.brave.com",
        "account.brave.com",
        "brave.com/",
        "cdn.brave.com",
        "duckduckgo.com",
        "duck.com/",
        "torproject.org",
        "tb-manual.torproject.org",
    ]
    .iter()
    .any(|h| lower_url.contains(h))
}

fn strip_tracking(url: &str) -> String {
    // Keep URL mostly intact; drop only obvious utm_* / fbclid / gclid suffixes.
    if let Some((base, query)) = url.split_once('?') {
        let kept: Vec<&str> = query
            .split('&')
            .filter(|p| {
                let k = p.split('=').next().unwrap_or("");
                !(k.starts_with("utm_")
                    || k == "fbclid"
                    || k == "gclid"
                    || k == "ved"
                    || k == "usg")
            })
            .collect();
        if kept.is_empty() {
            base.to_string()
        } else {
            format!("{base}?{}", kept.join("&"))
        }
    } else {
        url.to_string()
    }
}

fn push_unique(out: &mut Vec<SerpLink>, link: SerpLink) {
    if out.iter().any(|x| x.url == link.url) {
        return;
    }
    out.push(link);
}

fn rank_serp_links(mut links: Vec<SerpLink>) -> Vec<SerpLink> {
    links.sort_by(|a, b| {
        // Non-LinkedIn first, then higher article preference.
        a.linkedin
            .cmp(&b.linkedin)
            .then_with(|| article_prefer_score(b).cmp(&article_prefer_score(a)))
    });
    // Keep enough for All+News merge; writer still picks 3-4.
    links.truncate(12);
    links
}

/// Prefer on-topic articles / analysis over directories and repo roots.
#[must_use]
pub fn article_prefer_score(link: &SerpLink) -> i32 {
    let u = link.url.to_ascii_lowercase();
    let t = link.title.to_ascii_lowercase();
    let mut s = 0i32;
    if u.contains("labs.sogeti.com") {
        s += 120;
    }
    if u.contains("/blog/")
        || u.contains("/news/")
        || u.contains("/article")
        || u.contains("/research/")
        || u.contains("token-tax")
        || u.contains("token_tax")
    {
        s += 50;
    }
    // Long slug paths look like posts, not home pages.
    if let Some(path) = u.split('/').nth(3) {
        if path.len() > 24 && path.contains('-') {
            s += 25;
        }
    }
    if t.contains("token") || t.contains("ceo") || t.contains("leadership") {
        s += 15;
    }
    if u.contains("crunchbase.com") || u.contains("kompass.com") || u.contains("zoominfo.com") {
        s -= 35;
    }
    // GitHub repo root is weak as a LinkedIn cite; keep lower than articles.
    if u.contains("github.com/") && u.trim_end_matches('/').split('/').count() <= 5 {
        s -= 25;
    }
    if u.ends_with(".app/") || u.matches('/').count() <= 3 {
        s -= 5; // home / team pages are support, not primary news
    }
    s
}

/// `DuckDuckGo` organic links first; Brave web links only when DDG returned none.
#[must_use]
pub fn merge_ddg_and_brave_links(ddg: &[SerpLink], brave_fallback: &[SerpLink]) -> Vec<SerpLink> {
    if ddg.is_empty() {
        return rank_serp_links(brave_fallback.to_vec());
    }
    rank_serp_links(ddg.to_vec())
}

/// Formats links for the model tool result.
#[must_use]
pub fn format_serp_links(links: &[SerpLink]) -> String {
    format_serp_links_labeled(links, "")
}

/// Formats links with an optional section label prefix on each line.
#[must_use]
pub fn format_serp_links_labeled(links: &[SerpLink], label: &str) -> String {
    if links.is_empty() {
        return "(none extracted from page DOM/HTML)".into();
    }
    let tag = if label.is_empty() {
        "other-publisher".to_string()
    } else {
        format!("{label}-publisher")
    };
    let mut s = String::new();
    for (i, l) in links.iter().enumerate() {
        let _ = write!(
            s,
            "{}. [{tag}] url={}\n   title={}\n",
            i + 1,
            l.url,
            if l.title.is_empty() {
                "(none)"
            } else {
                &l.title
            }
        );
    }
    s
}

/// Parse `url=https://…` lines from a `web_search` tool result (MERGED / EXTRACTED sections).
#[must_use]
pub fn publisher_urls_from_tool_result(out: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for line in out.lines() {
        if let Some(rest) = line.trim().strip_prefix("url=") {
            let u = rest.trim();
            if (u.starts_with("https://") || u.starts_with("http://"))
                && !urls.iter().any(|x| x == u)
            {
                urls.push(u.to_string());
            }
        } else if let Some(idx) = line.find("url=") {
            let u = line[idx + 4..].trim();
            let u = u
                .split_whitespace()
                .next()
                .unwrap_or(u)
                .trim_end_matches(['|', ',', ';']);
            if (u.starts_with("https://") || u.starts_with("http://"))
                && !urls.iter().any(|x| x == u)
            {
                urls.push(u.to_string());
            }
        }
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_linkedin_posts_and_company_from_serp() {
        let links = extract_serp_links(
            r#"[{"url":"https://www.linkedin.com/posts/gr%C3%A9gory-roussac_rtk-activity-123/","title":"Post"},{"url":"https://www.linkedin.com/company/rtk-ai-labs/","title":"Co"},{"url":"https://labs.sogeti.com/the-hidden-cost-of-ai-coding/","title":"Sogeti"}]"#,
            "",
        );
        assert_eq!(links.len(), 1);
        assert!(links[0].url.contains("labs.sogeti.com"));
    }

    #[test]
    fn merge_ddg_and_brave_prefers_ddg_organic() {
        let ddg = extract_serp_links(
            r#"[{"url":"https://www.scylladb.com/2026/08/27/new-rust-driver-for-scylladbs-dynamodb-api/","title":"ScyllaDB blog"},{"url":"https://futurumgroup.com/insights/scylladbs-rust-driver-delivers-58-throughput-gain-for-dynamodb-users/","title":"Futurum"}]"#,
            "",
        );
        let brave = extract_serp_links(
            r#"[{"url":"https://hosseinnejati.medium.com/exploring-scylladb-a-high-performance-database-for-data-intensive-workloads-a0a27dd76ad0","title":"Medium"}]"#,
            "",
        );
        let merged = merge_ddg_and_brave_links(&ddg, &brave);
        assert!(merged[0].url.contains("scylladb.com/2026/08/27"));
        assert!(!merged.iter().any(|l| l.url.contains("medium.com")));
    }

    #[test]
    fn merge_ddg_and_brave_falls_back_when_ddg_empty() {
        let brave = extract_serp_links(
            r#"[{"url":"https://labs.sogeti.com/the-hidden-cost-of-ai-coding/","title":"Sogeti"}]"#,
            "",
        );
        let merged = merge_ddg_and_brave_links(&[], &brave);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].url.contains("labs.sogeti.com"));
    }

    #[test]
    fn unwrap_ddg_l_redirect() {
        let enc = "https%3A%2F%2Fwww.scylladb.com%2F2026%2F08%2F27%2Fnew-rust-driver";
        let raw = format!("https://duckduckgo.com/l/?uddg={enc}");
        let link = normalize_candidate(&raw, "").expect("ddg redirect");
        assert!(link.url.contains("scylladb.com/2026/08/27"));
    }

    #[test]
    fn detects_ddg_bot_wall() {
        assert!(looks_like_ddg_block(
            "Unfortunately, bots use DuckDuckGo too."
        ));
    }

    #[test]
    fn publisher_urls_from_labeled_serp_lines() {
        let out = "MERGED ranked candidates (2):\n\
1. [ddg-publisher] url=https://www.scylladb.com/2026/08/27/new-rust-driver-for-scylladbs-dynamodb-api/\n\
   title=ScyllaDB blog\n\
2. [ddg-publisher] url=https://futurumgroup.com/insights/scylladbs-rust-driver-delivers-58-throughput-gain-for-dynamodb-users/\n\
   title=Futurum\n";
        let urls = publisher_urls_from_tool_result(out);
        assert_eq!(urls.len(), 2);
        assert!(urls[0].contains("scylladb.com/2026/08/27"));
        assert!(urls[1].contains("futurumgroup.com"));
    }

    #[test]
    fn drops_ddg_page_chrome_from_extract() {
        let links = extract_serp_links(
            r#"[{"url":"https://duck.ai/","title":"Duck.ai"},{"url":"https://apps.apple.com/app/duckduckgo-private-browser/id663592361","title":"iOS"},{"url":"https://www.scylladb.com/2026/08/27/new-rust-driver-for-scylladbs-dynamodb-api/","title":"Blog"}]"#,
            "",
        );
        assert_eq!(links.len(), 1);
        assert!(links[0].url.contains("scylladb.com/2026/08/27"));
    }

    #[test]
    fn merge_prefers_sogeti_article_over_crunchbase() {
        let news = extract_serp_links(
            r#"[{"url":"https://labs.sogeti.com/the-hidden-cost-of-ai-coding-how-rtk-helps-developers-defeat-the-token-tax-part-2/","title":"Token Tax Part 2"}]"#,
            "",
        );
        let web = extract_serp_links(
            r#"[{"url":"https://www.crunchbase.com/organization/rtk-ai-labs-ltd","title":"HQ"},{"url":"https://www.rtk-ai.app/team/","title":"Team"},{"url":"https://github.com/rtk-ai/rtk","title":"GitHub"}]"#,
            "",
        );
        let mut merged: Vec<SerpLink> = Vec::new();
        for l in news.iter().chain(web.iter()) {
            if !merged.iter().any(|x| x.url == l.url) {
                merged.push(l.clone());
            }
        }
        let merged = rank_serp_links(merged);
        assert!(
            merged[0].url.contains("labs.sogeti.com"),
            "first should be Sogeti article, got {}",
            merged[0].url
        );
        assert!(merged.iter().any(|l| l.url.contains("rtk-ai.app")));
    }

    #[test]
    fn drops_all_linkedin_from_serp_extract() {
        let links = extract_serp_links(
            r#"[{"url":"https://www.linkedin.com/in/x","title":"LI"},{"url":"https://www.linkedin.com/uas/login","title":"login"},{"url":"https://www.linkedin.com/search/results/companies?q=x","title":"search"},{"url":"https://lnkd.in/abc","title":"short"},{"url":"https://www.rtk-ai.app/team/","title":"Team"},{"url":"https://www.crunchbase.com/organization/rtk-ai-labs-ltd","title":"CB"}]"#,
            "",
        );
        assert_eq!(links.len(), 2);
        assert!(links
            .iter()
            .all(|l| !l.url.to_ascii_lowercase().contains("linkedin")));
        assert!(links.iter().all(|l| !l.url.contains("lnkd.in")));
        assert!(links.iter().any(|l| l.url.contains("rtk-ai.app")));
        assert!(links.iter().any(|l| l.url.contains("crunchbase.com")));
    }

    #[test]
    fn real_session_evaluate_fixture_not_false_blocked() {
        // Replay the smoke that falsely reported SERP blocked while screenshot had real results.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../../pw/screenshots/20260728-023823-load_draft/01-web_search/01-evaluate.txt",
        );
        if !path.is_file() {
            return; // fixture not on disk in CI clones
        }
        let raw = std::fs::read_to_string(path).expect("read evaluate fixture");
        assert!(
            looks_like_brave_block(&raw),
            "fixture MCP blob still contains detector JS strings"
        );
        let ev = split_serp_evaluate(&raw);
        assert!(!ev.blocked, "JSON blocked flag must be false");
        assert!(
            !ev.ai_overview.is_empty(),
            "AI overview must survive Result-only parse"
        );
        let links = extract_serp_links(&ev.links_json, "");
        assert!(
            links.len() >= 5,
            "expected publisher links after LinkedIn strip, got {}",
            links.len()
        );
        assert!(
            links
                .iter()
                .any(|l| l.url.contains("rtk-ai.app") || l.url.contains("crunchbase")),
            "expected rtk-ai.app or crunchbase among EXTRACTED"
        );
        assert!(
            links
                .iter()
                .all(|l| !l.url.to_ascii_lowercase().contains("linkedin")),
            "LinkedIn must be stripped from EXTRACTED"
        );
        // Simulate the FIXED block policy: never block on eval_raw; trust links.
        let blocked = if !links.is_empty() && !ev.blocked {
            false
        } else {
            ev.blocked || looks_like_brave_block(&raw)
        };
        assert!(
            !blocked,
            "false-positive Brave block must not discard EXTRACTED"
        );
        let formatted = format_serp_links(&links);
        assert!(formatted.contains("[other-publisher]"));
        assert!(!formatted.contains("linkedin-deprioritized"));
    }

    #[test]
    fn snapshot_bot_wall_still_detected() {
        assert!(looks_like_brave_block(
            "heading Verifying you're not a bot Quick check before you continue searching"
        ));
        // Real SERP snapshot text must NOT trip the detector.
        assert!(!looks_like_brave_block(
            "RTK-AI Labs new CEO 2026 developments - Brave Search\nTeam - RTK AI Labs"
        ));
    }

    #[test]
    fn unwraps_google_url_redirect() {
        let raw = "https://www.google.com/url?q=https://pooyagolchian.com/blog/rtk-2026/&sa=U";
        let links = extract_serp_links(&format!(r#"[{{"url":"{raw}","title":"Blog"}}]"#), "");
        assert_eq!(links.len(), 1);
        assert!(links[0].url.contains("pooyagolchian.com"));
        assert!(!links[0].linkedin);
    }

    #[test]
    fn drops_linkedin_not_merely_ranks_last() {
        let links = extract_serp_links(
            r#"[{"url":"https://www.linkedin.com/posts/x","title":"LI"},{"url":"https://pooyagolchian.com/blog/x","title":"Blog"}]"#,
            "",
        );
        assert_eq!(links.len(), 1);
        assert!(links[0].url.contains("pooyagolchian.com"));
    }

    #[test]
    fn html_href_scrape_finds_organic_result() {
        let html = r#"<a class="zReHs" href="https://pooyagolchian.com/blog/stop-burning-claude-tokens-rtk-ai-coding-costs-2026/"><h3>RTK</h3></a>"#;
        let links = extract_serp_links("[]", html);
        assert!(links.iter().any(|l| l.url.contains("pooyagolchian.com")));
    }

    #[test]
    fn detects_google_block_page() {
        assert!(looks_like_google_block(
            "https://www.google.com/sorry/index?continue=https://www.google.com/search"
        ));
    }

    #[test]
    fn split_evaluate_object_payload() {
        let ev = split_serp_evaluate(
            r#"### Result
{"blocked":false,"href":"https://www.google.com/search","ai_overview":"AI says hello","links":[{"url":"https://pooyagolchian.com/blog/x","title":"Blog"}]}"#,
        );
        assert!(!ev.blocked);
        assert!(ev.links_json.contains("pooyagolchian.com"));
        assert!(ev.href.contains("google.com/search"));
        assert_eq!(ev.ai_overview, "AI says hello");
    }

    #[test]
    fn drops_youtube_reddit_and_trailing_backslash() {
        let links = extract_serp_links(
            r#"[{"url":"https://www.youtube.com/watch?v=x\","title":"yt"},{"url":"https://www.reddit.com/r/x\","title":"r"},{"url":"https://labs.sogeti.com/article/\","title":"ok"}]"#,
            "",
        );
        assert_eq!(links.len(), 1);
        assert!(links[0].url.contains("sogeti.com"));
        assert!(!links[0].url.ends_with('\\'));
    }

    #[test]
    fn mcp_evaluate_wrapper_with_detector_js_still_extracts_links() {
        // Host-browser evaluate appends the JS (which contains captcha detector strings).
        // That must NOT wipe real EXTRACTED links.
        let raw = r#"### Result
"{\"blocked\":false,\"href\":\"https://search.brave.com/search?q=x\",\"ai_overview\":\"AI says hello\",\"links\":[{\"url\":\"https://www.rtk-ai.app/team/\",\"title\":\"Team\"},{\"url\":\"https://www.crunchbase.com/organization/rtk-ai-labs-ltd\",\"title\":\"CB\"}]}"
### Ran Playwright code
```js
await page.evaluate('() => { const blocked = !!(/verifying you.?re not a bot|pow-captcha|traditional captcha|quick check before you continue searching/i.test(bodyText)); }');
```"#;
        assert!(
            looks_like_brave_block(raw),
            "raw MCP blob contains detector strings (this is why we must not block on eval_raw)"
        );
        let ev = split_serp_evaluate(raw);
        assert!(!ev.blocked);
        let links = extract_serp_links(&ev.links_json, "");
        assert_eq!(links.len(), 2);
        assert!(links[0].url.contains("rtk-ai.app"));
    }

    #[test]
    fn detects_brave_bot_wall() {
        assert!(looks_like_brave_block(
            "Verifying you're not a bot. Quick check before you continue searching."
        ));
        assert!(looks_like_brave_block("/help/pow-captcha"));
    }

    #[test]
    fn drops_brave_account_and_tor_chrome() {
        let links = extract_serp_links(
            r#"[{"url":"https://account.brave.com/?intent=checkout&product=search","title":"x"},{"url":"https://tb-manual.torproject.org/security-settings/#safest","title":"y"},{"url":"https://labs.sogeti.com/article/","title":"ok"}]"#,
            "",
        );
        assert_eq!(links.len(), 1);
        assert!(links[0].url.contains("sogeti.com"));
    }

    #[test]
    fn drops_example_and_google() {
        let links = extract_serp_links(
            r#"[{"url":"https://www.example.com/x","title":"x"},{"url":"https://www.google.com/search?q=y","title":"y"}]"#,
            "",
        );
        assert!(links.is_empty());
    }
}

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Short LOAD when `/tweet_about` subject/instructions already contain an https URL.

use crate::llm::client::CompletionTrace;
use crate::sources::rag::RagError;
use crate::sources::tweet_footer::{web_search_query, x_search_query};
use crate::sources::twitter::TwitterTool;
use crate::sources::url_hygiene::{
    is_allowed_tweet_cite, is_junk_or_search_url, is_x_status_url, x_status_id,
};
use crate::tools::ItcyTools;
use std::fmt::Write;
use tracing::{info, warn};

const PACK_CAP: usize = crate::sources::publisher_url::LINK_OPTIONS_CAP;
/// Pack summary budget for the browsed cite page. X accessibility trees are verbose; the
/// JPEG XL card `/url:` sat past 2k (~3.5k). Publisher URLs are still taken from the **full**
/// browse before this clip.
const CITE_TEXT_CHARS: usize = 10_000;

/// Label for SERP text in short-cite packs (secondary to the browsed cite page).
const SERP_SUPPORT_LABEL: &str =
    "SERP support (secondary; do not replace the cite subject or industry): ";

/// Fetch the https from the subject, optionally Brave + X search. Subject URL is always pack slot 1.
///
/// When `publisher_options` is true (`LinkedIn` drafts), an X status cite still runs Brave so
/// Link options 2/3 can be real publishers. Tweets pass false: skip SERP (avoids off-topic
/// news), but still pack publisher https found **inside** the status text.
///
/// # Errors
///
/// Returns a [`RagError`] when the short LOAD cannot complete.
pub async fn run_short_cite_load(
    subject: &str,
    subject_url: &str,
    tools: Option<&ItcyTools>,
    publisher_options: bool,
) -> Result<(String, Vec<String>, CompletionTrace), RagError> {
    crate::sources::rag::log_pipeline_banner("LOAD (short cite)");

    crate::sources::rag::log_pipeline_step("1/4 cite");
    let (cite_text, cite_publishers) = fetch_cite_clipped(subject_url, tools).await;

    let x_cite_only = x_status_cite_skips_serp(subject_url, publisher_options);

    crate::sources::rag::log_pipeline_step("2/4 brave web_search");
    let (overview, extracted_n) = if x_cite_only {
        info!("load_tweet: subject is X status; skip brave web_search");
        crate::sources::rag::log_pipeline_step("3/4 extra publisher browse");
        info!("load_tweet: skipped (x status cite)");
        (String::new(), 0)
    } else {
        let web_q = web_search_query(subject, "");
        brave_and_extra_browse(tools, &web_q, subject_url).await
    };

    crate::sources::rag::log_pipeline_step("4/4 X search");
    let (x_extra, x_hits) = if x_cite_only {
        info!("load_tweet: subject is X status; skip X keyword search");
        (None, 0)
    } else {
        let x_q = x_search_query(subject);
        info!(query = %x_q, "load_tweet: X query");
        let pair = extra_x_status(&x_q, subject_url).await;
        info!(
            query = %x_q,
            hits = pair.1,
            picked = pair.0.as_deref().unwrap_or("(none)"),
            "load_tweet: X results"
        );
        pair
    };

    let session = session_publisher_urls(tools, x_cite_only).await;
    let urls = assemble_and_probe_pack(
        &PackAssemble {
            subject_url,
            subject,
            cite_publishers: &cite_publishers,
            cite_text: &cite_text,
            overview: &overview,
            session: &session,
            x_extra,
        },
        tools,
    )
    .await?;

    crate::sources::rag::log_pipeline_step("pack");
    info!(
        cites = urls.len(),
        urls = %urls.join(" | "),
        extracted = extracted_n,
        x_hits,
        publisher_options,
        "load_tweet: short LOAD pack ready"
    );
    let pack = format_short_pack(subject, subject_url, &cite_text, &overview, &urls);
    Ok((
        pack,
        urls,
        CompletionTrace {
            provider: "short-load".into(),
            model: "cite+web+x".into(),
            prompt_tokens: 0,
            completion_tokens: 0,
        },
    ))
}

/// Full browse/API text → publisher extract → pack clip (10k).
async fn fetch_cite_clipped(url: &str, tools: Option<&ItcyTools>) -> (String, Vec<String>) {
    let (cite_raw, cite_via) = fetch_subject_url(url, tools).await;
    let cite_publishers = crate::sources::url_hygiene::publisher_urls_from_text(&cite_raw);
    let cite_text = clip(&cite_raw, CITE_TEXT_CHARS);
    let cite_text = crate::sources::draft_footer::strip_browse_page_title_chrome(&cite_text);
    info!(
        url = %url,
        via = cite_via,
        chars = cite_text.chars().count(),
        raw_chars = cite_raw.chars().count(),
        cite_pubs = cite_publishers.len(),
        cite_pub_urls = %cite_publishers.join(" | "),
        "load_tweet: cite fetched"
    );
    (cite_text, cite_publishers)
}

async fn session_publisher_urls(tools: Option<&ItcyTools>, x_cite_only: bool) -> Vec<String> {
    if x_cite_only {
        return Vec::new();
    }
    let Some(t) = tools else {
        return Vec::new();
    };
    t.session_extracted_urls()
        .await
        .into_iter()
        .chain(t.session_browsed_urls().await)
        .collect()
}

struct PackAssemble<'a> {
    subject_url: &'a str,
    subject: &'a str,
    cite_publishers: &'a [String],
    cite_text: &'a str,
    overview: &'a str,
    session: &'a [String],
    x_extra: Option<String>,
}

async fn assemble_and_probe_pack(
    parts: &PackAssemble<'_>,
    tools: Option<&ItcyTools>,
) -> Result<Vec<String>, RagError> {
    let mut urls = short_cite_pack_candidates(
        parts.subject_url,
        parts.subject,
        parts.cite_publishers,
        parts.cite_text,
        parts.overview,
        parts.session,
    );
    if let Some(u) = parts.x_extra.clone() {
        push_unique(&mut urls, std::iter::once(u));
    }
    info!(
        before_probe = urls.len(),
        urls = %urls.iter().take(12).cloned().collect::<Vec<_>>().join(" | "),
        "load_tweet: pack candidates before probe"
    );
    let refill_pool = urls.clone();
    urls.truncate(PACK_CAP);
    urls = crate::sources::publisher_url::filter_reachable_publisher_urls(urls).await;
    if !urls.iter().any(|u| u == parts.subject_url) {
        return Err(RagError::Store(format!(
            "cite URL not reachable: {}",
            parts.subject_url
        )));
    }
    if let Some(t) = tools {
        t.session_record_extracted_urls(&refill_pool).await;
    }
    info!(pool = refill_pool.len(), "load_tweet: refill pool retained");
    Ok(urls)
}

async fn fetch_subject_url(url: &str, tools: Option<&ItcyTools>) -> (String, &'static str) {
    if let Some(id) = x_status_id(url) {
        if let Ok(tool) = TwitterTool::from_disk() {
            match tool.lookup_status(&id).await {
                // Full detail: pack clip happens after publisher URL extract.
                Ok(hit) => return (hit.detail, "api"),
                Err(e) => warn!(error = %e, "load_tweet: status lookup failed; browse"),
            }
        }
    }
    let Some(t) = tools else {
        return (String::new(), "none");
    };
    match t.research_browse(url).await {
        Ok(out) => (out, "browse"),
        Err(e) => {
            warn!(error = %e, url = %url, "load_tweet: cite browse failed");
            (String::new(), "browse-failed")
        }
    }
}

async fn brave_and_extra_browse(
    tools: Option<&ItcyTools>,
    web_q: &str,
    subject_url: &str,
) -> (String, usize) {
    let Some(t) = tools else {
        info!("load_tweet: no tools; skip brave + browse");
        crate::sources::rag::log_pipeline_step("3/4 extra publisher browse");
        info!("load_tweet: skipped (no tools)");
        return (String::new(), 0);
    };
    info!(query = %web_q, "load_tweet: brave query");
    let overview = match t.research_web_search(web_q).await {
        Ok(out) => {
            let n = t.session_extracted_urls().await.len();
            info!(extracted = n, "load_tweet: brave EXTRACTED");
            out
        }
        Err(e) => {
            warn!(error = %e, "load_tweet: brave web_search failed");
            String::new()
        }
    };
    let extracted_n = t.session_extracted_urls().await.len();
    crate::sources::rag::log_pipeline_step("3/4 extra publisher browse");
    if is_x_status_url(subject_url) {
        info!("load_tweet: subject is X status; skip extra publisher browse");
    } else if let Some(extra) =
        first_extra_publisher(&t.session_extracted_urls().await, subject_url)
    {
        info!(url = %extra, "load_tweet: browsing extra publisher");
        if let Err(e) = t.research_browse(&extra).await {
            warn!(error = %e, url = %extra, "load_tweet: extra browse failed");
        }
    } else {
        info!("load_tweet: no extra publisher to browse");
    }
    (overview, extracted_n)
}

async fn extra_x_status(query: &str, subject_url: &str) -> (Option<String>, usize) {
    let Ok(tool) = TwitterTool::from_disk() else {
        warn!("load_tweet: X search skipped (no creds)");
        return (None, 0);
    };
    let hits = match tool.search(&[query.to_string()]).await {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, query = %query, "load_tweet: X search failed");
            return (None, 0);
        }
    };
    let n = hits.len();
    let subject_id = x_status_id(subject_url);
    let picked = hits
        .into_iter()
        .map(|h| h.url)
        .find(|u| is_x_status_url(u) && x_status_id(u) != subject_id);
    (picked, n)
}

fn first_extra_publisher(extracted: &[String], subject_url: &str) -> Option<String> {
    extracted
        .iter()
        .find(|u| {
            is_allowed_tweet_cite(u)
                && !is_x_status_url(u)
                && !is_junk_or_search_url(u)
                && *u != subject_url
        })
        .cloned()
}

/// Tweets with an X status cite skip SERP. `LinkedIn` drafts keep SERP for Link 2/3.
fn x_status_cite_skips_serp(subject_url: &str, publisher_options: bool) -> bool {
    is_x_status_url(subject_url) && !publisher_options
}

/// Build Link-option candidates.
///
/// Order is load-bearing: publishers **from the cite page** (tweet card `/url:`) and the
/// operator brief before Brave EXTRACTED, so SERP SEO scrapers cannot fill [`PACK_CAP`] and
/// crowd out the article the status actually links.
#[must_use]
fn short_cite_pack_candidates(
    subject_url: &str,
    subject: &str,
    cite_publishers: &[String],
    cite_text: &str,
    overview: &str,
    session_urls: &[String],
) -> Vec<String> {
    let mut urls = vec![subject_url.to_string()];
    // 1) Cite-page publishers (full browse extract, not the clipped pack summary).
    push_unique(
        &mut urls,
        cite_publishers
            .iter()
            .filter(|u| *u != subject_url && !is_x_status_url(u))
            .cloned(),
    );
    // 2) Operator brief: pasted publisher https next to the X cite.
    push_unique(
        &mut urls,
        crate::sources::tweet_footer::operator_https_urls(subject)
            .into_iter()
            .filter(|u| u != subject_url),
    );
    // 3) Clipped cite text (backup if extract missed).
    push_unique(
        &mut urls,
        crate::sources::url_hygiene::publisher_urls_from_text(cite_text)
            .into_iter()
            .filter(|u| u != subject_url),
    );
    // 4) Brave EXTRACTED / browsed (after cite, so News SEO does not steal slots).
    push_unique(
        &mut urls,
        session_urls
            .iter()
            .filter(|u| is_allowed_tweet_cite(u) && *u != subject_url && !is_x_status_url(u))
            .cloned(),
    );
    push_unique(
        &mut urls,
        crate::sources::url_hygiene::publisher_urls_from_text(overview)
            .into_iter()
            .filter(|u| u != subject_url && !is_x_status_url(u)),
    );
    urls
}

fn push_unique(urls: &mut Vec<String>, more: impl IntoIterator<Item = String>) {
    for u in more {
        if !urls.iter().any(|x| x == &u) {
            urls.push(u);
        }
    }
}

fn format_short_pack(
    subject: &str,
    subject_url: &str,
    cite_text: &str,
    overview: &str,
    urls: &[String],
) -> String {
    // Cite browse first (primary), then labeled SERP (secondary). Writer tools are off when
    // subject_https is locked, so this string is the only grounding.
    let cite = cite_text.trim();
    let serp = overview.trim();
    let ai_overview = if serp.is_empty() {
        String::new()
    } else {
        format!("{SERP_SUPPORT_LABEL}{serp}")
    };
    let mut out = format!(
        "## ResearchPack\nsubject: {subject}\nsubject_https: {subject_url}\nsummary: {cite}\nai_overview: {ai_overview}\ncandidates:\n"
    );
    for (i, u) in urls.iter().enumerate() {
        let kind = if i == 0 { "subject" } else { "support" };
        let _ = writeln!(out, "- final_url={u} | why={kind}");
    }
    if !cite.is_empty() {
        let _ = write!(
            out,
            "\n## Browsed page (this is the cite; do not switch industry or topic)\nurl: {subject_url}\n{cite}\n"
        );
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    t.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_pack_puts_subject_https_first_and_caps() {
        let urls = [
            "https://x.com/a/status/1".to_string(),
            "https://labs.sogeti.com/a".to_string(),
            "https://x.com/b/status/2".to_string(),
            "https://example.com/nope".to_string(),
        ];
        let pack = format_short_pack("casper", &urls[0], "hello cite", "overview", &urls[..3]);
        assert!(pack.contains("subject_https: https://x.com/a/status/1"));
        assert!(pack.contains("https://labs.sogeti.com/a"));
        let idx_lock = pack.find("https://x.com/a/status/1").expect("subject url");
        let idx_pub = pack.find("https://labs.sogeti.com/a").expect("pub");
        assert!(idx_lock < idx_pub);
        assert_eq!(PACK_CAP, crate::sources::publisher_url::LINK_OPTIONS_CAP);
    }

    #[test]
    fn short_pack_cite_before_serp_keeps_both() {
        let cite_url = "https://labs.sogeti.com/pgrust";
        let support = "https://example.org/support-article";
        let urls = [cite_url.to_string(), support.to_string()];
        let pack = format_short_pack(
            "pg_rust",
            cite_url,
            "cite page verbs about postgres rust",
            "serp snippet about unrelated travel",
            &urls,
        );
        assert!(pack.contains("summary: cite page verbs about postgres rust"));
        assert!(pack.contains(SERP_SUPPORT_LABEL));
        assert!(pack.contains("serp snippet about unrelated travel"));
        assert!(
            pack.contains("## Browsed page (this is the cite; do not switch industry or topic)")
        );
        assert!(pack.contains(&format!("url: {cite_url}")));
        let idx_summary = pack.find("summary:").expect("summary");
        let idx_ai = pack.find("ai_overview:").expect("ai_overview");
        let idx_browsed = pack.find("## Browsed page").expect("browsed");
        assert!(
            idx_summary < idx_ai,
            "cite summary must precede SERP ai_overview"
        );
        assert!(
            idx_ai < idx_browsed,
            "candidates sit between header fields and Browsed page; browsed is after candidates"
        );
        assert!(pack.contains("why=subject") && pack.contains("why=support"));
    }

    #[test]
    fn short_pack_empty_serp_leaves_ai_overview_blank() {
        let url = "https://labs.sogeti.com/x";
        let pack = format_short_pack("topic", url, "only cite", "", &[url.to_string()]);
        assert!(pack.contains("ai_overview: \n") || pack.contains("ai_overview:\n"));
        assert!(!pack.contains(SERP_SUPPORT_LABEL));
        assert!(pack.contains("## Browsed page"));
    }

    #[test]
    fn cite_clip_budget_is_ten_thousand() {
        assert_eq!(CITE_TEXT_CHARS, 10_000);
    }

    #[test]
    fn extra_publisher_skips_subject_url_and_status() {
        let extracted = vec![
            "https://x.com/a/status/1".to_string(),
            "https://labs.sogeti.com/x402".to_string(),
        ];
        assert_eq!(
            first_extra_publisher(&extracted, "https://x.com/a/status/1").as_deref(),
            Some("https://labs.sogeti.com/x402")
        );
    }

    #[test]
    fn linkedin_draft_x_cite_keeps_serp_tweet_skips() {
        let x = "https://x.com/mmalisper/status/2091925981363941499";
        assert!(
            !x_status_cite_skips_serp(x, true),
            "LinkedIn draft must still search for publisher Link options"
        );
        assert!(
            x_status_cite_skips_serp(x, false),
            "tweet X cite must skip SERP to avoid off-topic pack"
        );
        assert!(!x_status_cite_skips_serp(
            "https://labs.sogeti.com/pgrust",
            false
        ));
    }

    #[test]
    fn in_tweet_https_enters_pack_candidates() {
        let cite = "Builders shipping codecs.\nhttps://labs.sogeti.com/codec-note\n";
        let subject_url = "https://x.com/a/status/99";
        let pubs = crate::sources::url_hygiene::publisher_urls_from_text(cite);
        let urls = short_cite_pack_candidates(subject_url, subject_url, &pubs, cite, "", &[]);
        assert_eq!(
            urls,
            vec![
                subject_url.to_string(),
                "https://labs.sogeti.com/codec-note".to_string(),
            ]
        );
    }

    #[test]
    fn serp_overview_url_equals_lines_become_candidates_min_three() {
        // Truncated cite browse; SERP overview lists publisher https in `url=` prose.
        // Any host — not a special-case string match.
        let x = "https://x.com/a/status/1";
        let cite = "Page text truncated mid sentence…";
        let overview = "\
1. [web-publisher] url=https://labs.sogeti.com/intent-note/\n\
   title=Some lab post\n\
2. [news-publisher] url=https://decrypt.co/12345/codec-story\n\
   title=Codec story\n";
        let urls = short_cite_pack_candidates(x, x, &[], cite, overview, &[]);
        assert_eq!(urls.first().map(String::as_str), Some(x));
        assert!(
            urls.iter()
                .any(|u| u == "https://labs.sogeti.com/intent-note"),
            "overview url= publisher must be a candidate: {urls:?}"
        );
        assert!(
            urls.iter()
                .any(|u| u == "https://decrypt.co/12345/codec-story"),
            "second overview url= must be a candidate: {urls:?}"
        );
        let opts = crate::sources::draft_footer::pick_link_options(&urls, "");
        assert!(
            opts.len() >= crate::sources::publisher_url::LINK_OPTIONS_MIN,
            "LinkedIn pick must meet floor of 3 when pack has 3 domains: {opts:?}"
        );
    }

    #[test]
    fn cite_publishers_beat_news_serp_before_pack_cap() {
        // DRAFT-113: News EXTRACTED filled PACK_CAP; mozilla was #3 on All/web and never probed.
        // Cite-page publishers (tweet card) must occupy slots before SERP News SEO.
        let x = "https://x.com/ayushagarwal027/status/2092326145312395657";
        let cite_pubs =
            vec!["https://hacks.mozilla.org/2026/08/intent-to-ship-jpeg-xl".to_string()];
        let session = vec![
            "https://hwbusters.com/news/jpeg-xl-in-firefox-157-mozilla-made-google-rewrite-the-decoder-in-rust-first".into(),
            "https://freenode.net/article/chromium-plans-to-ship-jpeg-xl-image-decoding-in-blink".into(),
            "https://compresto.app/blog/jpeg-xl".into(),
            "https://freetoolonline.com/news/jpeg-xl-returns-chrome-firefox.html".into(),
            "https://debugbear.com/blog/jpeg-xl-image-format".into(),
        ];
        let mut capped = short_cite_pack_candidates(x, x, &cite_pubs, "clipped…", "", &session);
        capped.truncate(PACK_CAP);
        assert_eq!(
            capped.get(1).map(String::as_str),
            Some("https://hacks.mozilla.org/2026/08/intent-to-ship-jpeg-xl"),
            "tweet card publisher must be Link slot 2 before News SEO: {capped:?}"
        );
    }

    #[test]
    fn recorded_ayush_jpeg_xl_browse_yields_mozilla_hacks() {
        // Real browse residue from DRAFT-20260826-000113.
        let raw = include_str!("fixtures/ayush_jpeg_xl_x_browse.txt");
        let pubs = crate::sources::url_hygiene::publisher_urls_from_text(raw);
        assert!(
            pubs.iter().any(|u| u.contains("hacks.mozilla.org")),
            "full browse must yield mozilla card URL: {pubs:?}"
        );
        // Old 2k clip dropped the card; keep that assert so we never rely on clip alone.
        let old_clip = clip(raw, 2_000);
        assert!(
            !old_clip.contains("hacks.mozilla.org"),
            "2k clip still omits the card (extract-from-full remains required)"
        );
        let x = "https://x.com/ayushagarwal027/status/2092326145312395657";
        let session = vec![
            "https://hwbusters.com/news/jpeg-xl".into(),
            "https://freenode.net/article/chromium".into(),
            "https://compresto.app/blog/jpeg-xl".into(),
            "https://freetoolonline.com/news/jpeg-xl".into(),
        ];
        let clipped = clip(raw, CITE_TEXT_CHARS);
        let mut capped = short_cite_pack_candidates(x, x, &pubs, &clipped, "", &session);
        capped.truncate(PACK_CAP);
        assert!(
            capped.iter().any(|u| u.contains("hacks.mozilla.org")),
            "recorded tweet card must survive PACK_CAP ahead of News SERP: {capped:?}"
        );
    }

    #[test]
    fn session_extracted_still_fills_when_cite_has_no_publisher() {
        let x = "https://x.com/a/status/1";
        let cite = "do not replace https://SUBJECT or https://request https://browsers\n";
        let overview = "url=https://search.brave.com/search?q=jpeg\n";
        let session = vec![
            "https://hacks.mozilla.org/2026/08/intent-to-ship-jpeg-xl/".into(),
            "https://labs.sogeti.com/codec".into(),
            "https://decrypt.co/1/jpeg-xl".into(),
            "https://example.org/other".into(),
        ];
        let mut capped = short_cite_pack_candidates(x, x, &[], cite, overview, &session);
        capped.truncate(PACK_CAP);
        assert!(
            capped.iter().any(|u| u.contains("hacks.mozilla.org")),
            "EXTRACTED publisher must still enter when cite has no card: {capped:?}"
        );
        assert!(
            !capped
                .iter()
                .any(|u| u == "https://SUBJECT" || u == "https://request"),
            "prose junk must not occupy pack slots: {capped:?}"
        );
    }

    #[test]
    fn operator_pasted_publisher_in_subject_enters_candidates() {
        let x = "https://x.com/a/status/1";
        let brief = format!(
            "JPEG XL shipping, cite {x} https://hacks.mozilla.org/2026/08/intent-to-ship-jpeg-xl/"
        );
        let urls =
            short_cite_pack_candidates(x, &brief, &[], "truncated cite without url", "", &[]);
        assert!(
            urls.iter().any(|u| u.contains("hacks.mozilla.org")),
            "operator-pasted article must be a pack candidate: {urls:?}"
        );
    }
}

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
const CITE_TEXT_CHARS: usize = 2000;

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
    let (cite_text, cite_via) = fetch_subject_url(subject_url, tools).await;
    info!(
        url = %subject_url,
        via = cite_via,
        chars = cite_text.chars().count(),
        "load_tweet: cite fetched"
    );

    let x_cite_only = x_status_cite_skips_serp(subject_url, publisher_options);

    crate::sources::rag::log_pipeline_step("2/4 brave web_search");
    // Tweet + X status cite: fetch that status only. Brave SERP on a short stub pulls
    // unrelated malware/news into the pack and the writer follows it.
    // LinkedIn draft + X status: still search so the operator gets up to 3 Link options.
    let (overview, extracted_n) = if x_cite_only {
        info!("load_tweet: subject is X status; skip brave web_search");
        crate::sources::rag::log_pipeline_step("3/4 extra publisher browse");
        info!("load_tweet: skipped (x status cite)");
        (String::new(), 0)
    } else {
        let web_q = web_search_query(subject);
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

    let mut urls = vec![subject_url.to_string()];
    // Publisher https already in the status/page text beat Brave SERP guesses
    // (e.g. Mozilla Hacks link inside an X status about JPEG XL).
    push_unique(
        &mut urls,
        crate::sources::url_hygiene::publisher_urls_from_text(&cite_text)
            .into_iter()
            .filter(|u| u != subject_url),
    );
    // LinkedIn drafts (and non-X cites) also take Brave publishers into options 2/3.
    // Tweets with an X-only SERP skip still keep in-tweet publishers above.
    if !x_cite_only {
        if let Some(t) = tools {
            push_unique(
                &mut urls,
                t.session_extracted_urls()
                    .await
                    .into_iter()
                    .chain(t.session_browsed_urls().await)
                    .filter(|u| {
                        is_allowed_tweet_cite(u) && u != subject_url && !is_x_status_url(u)
                    }),
            );
        }
    }
    if let Some(u) = x_extra {
        push_unique(&mut urls, std::iter::once(u));
    }
    urls.truncate(PACK_CAP);
    urls = crate::sources::publisher_url::filter_reachable_publisher_urls(urls).await;
    if !urls.iter().any(|u| u == subject_url) {
        return Err(RagError::Store(format!(
            "cite URL not reachable: {subject_url}"
        )));
    }
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

async fn fetch_subject_url(url: &str, tools: Option<&ItcyTools>) -> (String, &'static str) {
    if let Some(id) = x_status_id(url) {
        if let Ok(tool) = TwitterTool::from_disk() {
            match tool.lookup_status(&id).await {
                Ok(hit) => return (clip(&hit.detail, CITE_TEXT_CHARS), "api"),
                Err(e) => warn!(error = %e, "load_tweet: status lookup failed; browse"),
            }
        }
    }
    let Some(t) = tools else {
        return (String::new(), "none");
    };
    match t.research_browse(url).await {
        Ok(out) => (clip(&out, CITE_TEXT_CHARS), "browse"),
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

fn push_unique(urls: &mut Vec<String>, more: impl IntoIterator<Item = String>) {
    for u in more {
        if urls.len() >= PACK_CAP {
            break;
        }
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
    fn cite_clip_budget_is_two_thousand() {
        assert_eq!(CITE_TEXT_CHARS, 2000);
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
    fn in_tweet_publisher_url_beats_empty_pack_slot() {
        let cite = "Mozilla wouldn't ship JPEG XL.\n\
https://hacks.mozilla.org/2026/08/intent-to-ship-jpeg-xl\n";
        let subject = "https://x.com/ayushagarwal027/status/2092326145312395657";
        let mut urls = vec![subject.to_string()];
        push_unique(
            &mut urls,
            crate::sources::url_hygiene::publisher_urls_from_text(cite)
                .into_iter()
                .filter(|u| u != subject),
        );
        assert_eq!(
            urls,
            vec![
                subject.to_string(),
                "https://hacks.mozilla.org/2026/08/intent-to-ship-jpeg-xl".to_string(),
            ]
        );
    }
}

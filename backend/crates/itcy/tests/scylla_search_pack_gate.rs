// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Offline + live gate: `ScyllaDB` subject → SERP → probe → pack (no LLM).
//!
//! Run live: `cargo test -p itcy --test scylla_search_pack_gate -- --ignored --nocapture`

use itcy::sources::draft_footer::pick_link_options;
use itcy::sources::publisher_url::{evaluate_publisher_probe, filter_reachable_publisher_urls};
use itcy::sources::tweet_footer::web_search_query;

const SCYLLA_SUBJECT: &str =
    "ScyllaDB Rustlang driver for ScyllaDB Alternator that achieves ~58% higher throughput than the AWS SDK driver";

const BLOG_2026: &str =
    "https://www.scylladb.com/2026/08/27/new-rust-driver-for-scylladbs-dynamodb-api/";
const FUTURUM: &str =
    "https://futurumgroup.com/insights/scylladbs-rust-driver-delivers-58-throughput-gain-for-dynamodb-users/";

/// Pack merge order: SERP extracted first, then browsed, then LLM pack text.
fn pack_urls_from_load(
    from_pack: &[String],
    browsed: &[String],
    extracted: &[String],
) -> Vec<String> {
    use itcy::sources::publisher_url::LINK_OPTIONS_CAP;
    use itcy::sources::url_hygiene::filter_publisher_urls;
    let mut pack_urls: Vec<String> = Vec::new();
    merge_urls_cap(
        &mut pack_urls,
        filter_publisher_urls(extracted),
        LINK_OPTIONS_CAP,
    );
    merge_urls_cap(&mut pack_urls, browsed.iter().cloned(), LINK_OPTIONS_CAP);
    merge_urls_cap(
        &mut pack_urls,
        filter_publisher_urls(from_pack),
        LINK_OPTIONS_CAP,
    );
    pack_urls
}

fn merge_urls_cap(
    pack_urls: &mut Vec<String>,
    extras: impl IntoIterator<Item = String>,
    cap: usize,
) {
    for u in extras {
        if !pack_urls.iter().any(|x| x == &u) && pack_urls.len() < cap {
            pack_urls.push(u);
        }
    }
}

#[test]
fn scylla_query_is_full_subject_not_first_token() {
    assert_eq!(web_search_query(SCYLLA_SUBJECT, ""), SCYLLA_SUBJECT);
    assert_ne!(web_search_query(SCYLLA_SUBJECT, ""), "ScyllaDB");
}

#[tokio::test]
async fn scylla_obvious_urls_pass_publisher_probe() {
    let client = reqwest::Client::builder()
        .user_agent("ITCy/0.1 (+https://interchouette.net; publisher URL probe)")
        .timeout(std::time::Duration::from_secs(25))
        .build()
        .expect("client");
    for url in [BLOG_2026, FUTURUM] {
        let res = client
            .get(url)
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {url} failed: {e}"));
        let status = res.status().as_u16();
        let mut body = res
            .text()
            .await
            .unwrap_or_else(|e| panic!("body {url}: {e}"));
        if body.len() > 256_000 {
            body.truncate(256_000);
        }
        evaluate_publisher_probe(status, &body)
            .unwrap_or_else(|reason| panic!("probe rejected {url}: {reason}"));
    }
}

#[tokio::test]
async fn scylla_pack_pipeline_keeps_three_links_after_probe() {
    let extracted = vec![
        BLOG_2026.to_string(),
        FUTURUM.to_string(),
        "https://university.scylladb.com/courses/scylla-alternator/".to_string(),
        "https://www.scylladb.com/2025/03/26/scylladb-rust-driver-1-0/".to_string(),
    ];
    let pack = pack_urls_from_load(&[], &[], &extracted);
    assert!(pack.len() >= 3, "{pack:?}");
    let reachable = filter_reachable_publisher_urls(pack).await;
    assert!(
        reachable.len() >= 3,
        "expected >=3 reachable after probe; got {reachable:?}"
    );
    let lower = reachable
        .iter()
        .map(|u| u.to_ascii_lowercase())
        .collect::<Vec<_>>();
    assert!(
        lower
            .iter()
            .any(|u| u.contains("2026/08/27/new-rust-driver")),
        "missing 2026 blog: {reachable:?}"
    );
    assert!(
        lower
            .iter()
            .any(|u| u.contains("futurumgroup.com/insights/scylladbs-rust-driver")),
        "missing futurum: {reachable:?}"
    );
    let opts = pick_link_options(&reachable, "");
    assert!(opts.len() >= 3, "{opts:?}");
    assert!(!opts.iter().any(|u| u.contains("2025/03/26")), "{opts:?}");
}

#[tokio::test]
#[ignore = "live SERP; run with --ignored"]
async fn scylla_live_serp_yields_obvious_first_links() {
    use itcy::tools::{resolve_host_browser_cmd, HostBrowser};

    let query = web_search_query(SCYLLA_SUBJECT, "");
    assert_eq!(query, SCYLLA_SUBJECT);

    let browser = HostBrowser::spawn(&resolve_host_browser_cmd())
        .await
        .expect("browser");
    let out = browser.web_search(&query, None).await.expect("web_search");
    let lower = out.to_ascii_lowercase();
    assert!(
        lower.contains("scylladb.com/2026/08/27/new-rust-driver"),
        "SERP missing 2026 ScyllaDB blog; out={out}"
    );
    assert!(
        lower.contains("futurumgroup.com/insights/scylladbs-rust-driver"),
        "SERP missing Futurum insight; out={out}"
    );

    let extracted = itcy::tools::publisher_urls_from_tool_result(&out);
    assert!(
        extracted.len() >= 3,
        "expected >=3 SERP links; got {extracted:?}"
    );

    let pack = pack_urls_from_load(&[], &[], &extracted);
    let reachable = filter_reachable_publisher_urls(pack).await;
    assert!(reachable.len() >= 3, "reachable pack: {reachable:?}");
    let rl = reachable
        .iter()
        .map(|u| u.to_ascii_lowercase())
        .collect::<Vec<_>>();
    assert!(
        rl.iter().any(|u| u.contains("2026/08/27/new-rust-driver")),
        "{reachable:?}"
    );
    assert!(
        rl.iter()
            .any(|u| u.contains("futurumgroup.com/insights/scylladbs-rust-driver")),
        "{reachable:?}"
    );
}

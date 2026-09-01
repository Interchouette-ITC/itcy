// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Daily subject digest store + builder (live hubs + Twitter tool).

use crate::sources::ingest::{HttpPageFetcher, HttpThenPublicPlaywright, PageFetcher};
use crate::sources::live_sites::{load_live_sites, LiveSite};
use crate::sources::twitter::{TwitterHit, TwitterTool};
use crate::sqlite::open_configured;
use chrono::{Days, Local, NaiveDate};
use rusqlite::params;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tracing::{info, warn};

const SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/digest_schema.sql"
));
const INSERT_DIGEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/digest_insert.sql"
));
const INSERT_ITEM: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/digest_item_insert.sql"
));
const GET_DIGEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/digest_get.sql"
));
const LATEST_OPEN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/digest_latest_open.sql"
));
const LIST_ITEMS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/digest_items_list.sql"
));
const RECENT_ITEM_KEYS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sql/digest_recent_item_keys.sql"
));

const MAX_SITE_ITEMS: usize = 20;
const MAX_TWITTER_ITEMS: usize = 20;
const MAX_FOR_YOU_ITEMS: usize = 20;
const MAX_FOLLOWING_ITEMS: usize = 20;
const MAX_ITC_ITEMS: usize = 10;
const MAX_PER_HUB: usize = 2;
const HUB_CANDIDATES: usize = 12;
/// Scan ceiling per hub extract (`DoS` guard only; not a freshness knob).
const HUB_LINK_SCAN_MAX: usize = 100;
/// Over-fetch press so listing drops after blurbs can still fill PRESS 20.
const PRESS_POOL: usize = 40;
/// Search-lane author spam cap (home lanes have no author hard-cap).
const MAX_SEARCH_PER_AUTHOR: usize = 2;
/// Fair mix: at most this many hits kept per planned search query in the first pass.
const MAX_PER_SEARCH_QUERY: usize = 3;
/// Prior calendar days whose digest URLs/titles are excluded from today's build.
const SEEN_LOOKBACK_DAYS: u64 = 7;

/// One numbered digest choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestItem {
    pub idx: i32,
    pub title: String,
    pub url: Option<String>,
    pub subject: String,
    pub lane: String,
    pub weight: i32,
    pub detail: String,
}

/// Operator brief for `/propose_draft` / `/propose_tweet` from a digest row.
///
/// Uses the full card `detail` (not the truncated `subject` column alone) plus the
/// item URL so LOAD cannot "search" a short stub and attach off-topic SERP cites.
#[must_use]
pub fn digest_propose_brief(it: &DigestItem) -> (String, String) {
    let detail = it.detail.trim();
    let subject_col = it.subject.trim();
    let title = it.title.trim();
    let topic = detail
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .or(if subject_col.is_empty() {
            None
        } else {
            Some(subject_col)
        })
        .or(if title.is_empty() { None } else { Some(title) })
        .unwrap_or("digest item")
        .to_string();
    let mut instructions = if detail.is_empty() {
        subject_col.to_string()
    } else {
        detail.to_string()
    };
    if let Some(url) = it.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        if !instructions.contains(url) {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(url);
        }
    }
    (topic, instructions)
}

/// Load a digest and pick items for `/propose_draft` / `/propose_tweet`.
///
/// # Errors
///
/// Returns an operator-facing message when the digest is missing or indices are invalid.
pub fn load_digest_pick(
    db_path: &std::path::Path,
    digest_id: Option<&str>,
    indices: &[i32],
    cmd: &str,
) -> Result<(DigestRecord, Vec<DigestItem>), String> {
    let rec = match digest_id {
        Some(id) => match get_digest(db_path, id) {
            Ok(Some(r)) => r,
            Ok(None) => return Err(format!("No digest `{id}` in runtime.db.")),
            Err(e) => return Err(format!("`{cmd}` failed: {e}")),
        },
        None => match latest_open_digest(db_path) {
            Ok(Some(r)) => r,
            Ok(None) => return Err("No open digest.\n\nRun /daily_digest first.".into()),
            Err(e) => return Err(format!("`{cmd}` failed: {e}")),
        },
    };
    let picked: Vec<DigestItem> = pick_items(&rec, indices)
        .map_err(|e| format!("`{cmd}` failed: {e}"))?
        .into_iter()
        .cloned()
        .collect();
    Ok((rec, picked))
}

/// Stored digest header + items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestRecord {
    pub digest_id: String,
    pub status: String,
    pub created_at: String,
    pub items: Vec<DigestItem>,
}

/// Candidate before numbering.
#[derive(Debug, Clone)]
struct Candidate {
    title: String,
    url: Option<String>,
    subject: String,
    lane: String,
    weight: i32,
    detail: String,
    /// Search query bucket for fair TWITTER mix (empty for follows / press).
    query: String,
}

/// Digest errors.
#[derive(Debug, Error)]
pub enum DigestError {
    #[error("digest db: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("digest: {0}")]
    Other(String),
}

/// Ensures schema on the state DB.
///
/// # Errors
///
/// Returns [`DigestError`] on `SQLite` failure.
pub fn ensure_digest_schema(db_path: &Path) -> Result<(), DigestError> {
    let conn = open_configured(db_path)?;
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

/// Allocates `DIGEST-YYYYMMDD-NNNNNN`.
///
/// # Errors
///
/// Returns [`DigestError`] on `SQLite` failure.
pub fn next_digest_id(db_path: &Path) -> Result<String, DigestError> {
    ensure_digest_schema(db_path)?;
    let conn = open_configured(db_path)?;
    let ord: i64 = conn.query_row(
        "SELECT next_ord FROM digest_code_seq WHERE id = 1",
        [],
        |r| r.get(0),
    )?;
    let next = ord.saturating_add(1);
    conn.execute(
        "UPDATE digest_code_seq SET next_ord = ?1 WHERE id = 1",
        params![next],
    )?;
    let day = Local::now().format("%Y%m%d");
    Ok(format!("DIGEST-{day}-{next:06}"))
}

/// Persists a digest + items.
///
/// # Errors
///
/// Returns [`DigestError`] on `SQLite` failure.
pub fn insert_digest(
    db_path: &Path,
    digest_id: &str,
    items: &[DigestItem],
) -> Result<(), DigestError> {
    ensure_digest_schema(db_path)?;
    let conn = open_configured(db_path)?;
    let created = Local::now().to_rfc3339();
    conn.execute(INSERT_DIGEST, params![digest_id, "open", created])?;
    for it in items {
        conn.execute(
            INSERT_ITEM,
            params![
                digest_id, it.idx, it.title, it.url, it.subject, it.lane, it.weight, it.detail,
            ],
        )?;
    }
    Ok(())
}

/// Loads one digest by id.
///
/// # Errors
///
/// Returns [`DigestError`] on `SQLite` failure.
pub fn get_digest(db_path: &Path, digest_id: &str) -> Result<Option<DigestRecord>, DigestError> {
    ensure_digest_schema(db_path)?;
    let conn = open_configured(db_path)?;
    let header = conn.query_row(GET_DIGEST, params![digest_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    });
    let (digest_id, status, created_at) = match header {
        Ok(h) => h,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(Some(DigestRecord {
        digest_id: digest_id.clone(),
        status,
        created_at,
        items: load_items(&conn, &digest_id)?,
    }))
}

/// Latest open digest.
///
/// # Errors
///
/// Returns [`DigestError`] on `SQLite` failure.
pub fn latest_open_digest(db_path: &Path) -> Result<Option<DigestRecord>, DigestError> {
    ensure_digest_schema(db_path)?;
    let conn = open_configured(db_path)?;
    let header = conn.query_row(LATEST_OPEN, [], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    });
    let (digest_id, status, created_at) = match header {
        Ok(h) => h,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(Some(DigestRecord {
        digest_id: digest_id.clone(),
        status,
        created_at,
        items: load_items(&conn, &digest_id)?,
    }))
}

fn load_items(
    conn: &rusqlite::Connection,
    digest_id: &str,
) -> Result<Vec<DigestItem>, DigestError> {
    let mut stmt = conn.prepare(LIST_ITEMS)?;
    let rows = stmt.query_map(params![digest_id], |r| {
        Ok(DigestItem {
            idx: r.get(0)?,
            title: r.get(1)?,
            url: r.get(2)?,
            subject: r.get(3)?,
            lane: r.get(4)?,
            weight: r.get(5)?,
            detail: r.get(6)?,
        })
    })?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

/// Top-level Slack overview (`DIGEST` fence; per-item propose lines sit under each choice).
#[must_use]
pub fn format_digest_overview(rec: &DigestRecord) -> String {
    format!("```\n{id}\n```", id = rec.digest_id)
}

/// Grey empty code bar posted **between** digest items (NBSP so Slack keeps the fence).
pub const DIGEST_ITEM_GREY_BAR: &str = "```\n\u{00a0}\n```";

/// One digest choice: number, title, grey summary, URL, then clear `/propose_*` lines to copy.
#[must_use]
pub fn format_digest_item(it: &DigestItem, digest_id: &str) -> String {
    let url = it.url.as_deref().unwrap_or("-");
    let head = format!(
        "`{idx}` _ *{title}*",
        idx = it.idx,
        title = item_headline(it)
    );
    let mut parts = vec![head];
    if let Some(body) = item_grey_body(it) {
        parts.push(fence_block(&body));
    }
    parts.push(url.to_string());
    parts.push(format!(
        "/propose_draft {digest_id}, {idx}\n/propose_tweet {digest_id}, {idx}",
        idx = it.idx,
    ));
    parts.join("\n\n")
}

/// Insert [`DIGEST_ITEM_GREY_BAR`] between consecutive item messages.
#[must_use]
pub fn with_item_grey_bars(items: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(items.len().saturating_mul(2).saturating_sub(1));
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            out.push(DIGEST_ITEM_GREY_BAR.into());
        }
        out.push(it.clone());
    }
    out
}

fn is_x_lane(lane: &str) -> bool {
    lane == "twitter" || lane == "following" || lane == "for_you"
}

fn is_home_lane(lane: &str) -> bool {
    lane == "following" || lane == "for_you"
}

fn is_itc_lane(lane: &str) -> bool {
    lane == "itc_draft" || lane == "itc_tweet"
}

fn item_grey_body(it: &DigestItem) -> Option<String> {
    if is_x_lane(&it.lane) {
        let t = tweet_body(it);
        return (!t.is_empty()).then_some(t);
    }
    let t = it.detail.trim();
    if is_itc_lane(&it.lane) {
        return (!t.is_empty()).then(|| t.to_string());
    }
    crate::sources::html::looks_like_article_blurb(t).then(|| t.to_string())
}

fn fence_block(text: &str) -> String {
    let safe = text.replace("```", "'''");
    format!("```\n{safe}\n```")
}

fn item_headline(it: &DigestItem) -> String {
    if is_x_lane(&it.lane) {
        twitter_headline(&it.title)
    } else if is_itc_lane(&it.lane) {
        it.title.trim().to_string()
    } else {
        display_title(&it.title)
    }
}

fn twitter_headline(title: &str) -> String {
    if let Some((head, _)) = title.split_once(" · ") {
        let t = head.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    title
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ")
}

fn tweet_body(it: &DigestItem) -> String {
    let detail = it.detail.trim();
    if !detail.is_empty() && !is_twitter_lane_hint(detail) {
        return detail.to_string();
    }
    if let Some((_, rest)) = it.title.split_once(" · ") {
        let rest = rest.trim();
        if !rest.is_empty() {
            return rest.to_string();
        }
    }
    it.title.trim().to_string()
}

fn is_twitter_lane_hint(s: &str) -> bool {
    s.starts_with("twitter:") || s.starts_with("twitter-pw:")
}

fn display_title(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_end_matches(".html")
        .trim_end_matches(".htm")
        .trim()
        .replace(['-', '_'], " ");
    title_case(&trimmed)
}

fn title_case(s: &str) -> String {
    let mut out = String::new();
    for (i, word) in s.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.extend(chars.flat_map(char::to_lowercase));
        }
    }
    out
}

/// Slack channel layout: overview, then press, dual home, Twitter search, Interchouette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestSlackPost {
    pub overview: String,
    pub press_title: String,
    pub press_items: Vec<String>,
    pub for_you_title: String,
    pub for_you_items: Vec<String>,
    pub following_title: String,
    pub following_items: Vec<String>,
    pub twitter_title: String,
    pub twitter_items: Vec<String>,
    pub itc_title: String,
    pub itc_items: Vec<String>,
}

/// Build the Slack layout for a digest (five content threads after overview).
#[must_use]
pub fn digest_slack_post(rec: &DigestRecord) -> DigestSlackPost {
    let press: Vec<&DigestItem> = rec
        .items
        .iter()
        .filter(|i| !is_x_lane(&i.lane) && !is_itc_lane(&i.lane))
        .collect();
    let for_you: Vec<&DigestItem> = rec.items.iter().filter(|i| i.lane == "for_you").collect();
    let following: Vec<&DigestItem> = rec.items.iter().filter(|i| i.lane == "following").collect();
    let tweets: Vec<&DigestItem> = rec.items.iter().filter(|i| i.lane == "twitter").collect();
    let itc: Vec<&DigestItem> = rec.items.iter().filter(|i| is_itc_lane(&i.lane)).collect();
    DigestSlackPost {
        overview: format_digest_overview(rec),
        press_title: format!("```\nPRESS {n}\n```", n = press.len()),
        press_items: press
            .into_iter()
            .map(|it| format_digest_item(it, &rec.digest_id))
            .collect(),
        for_you_title: format!("```\nFOLLOWS FOR YOU {n}\n```", n = for_you.len()),
        for_you_items: for_you
            .into_iter()
            .map(|it| format_digest_item(it, &rec.digest_id))
            .collect(),
        following_title: format!("```\nFOLLOWING {n}\n```", n = following.len()),
        following_items: following
            .into_iter()
            .map(|it| format_digest_item(it, &rec.digest_id))
            .collect(),
        twitter_title: format!("```\nTWITTER {n}\n```", n = tweets.len()),
        twitter_items: tweets
            .into_iter()
            .map(|it| format_digest_item(it, &rec.digest_id))
            .collect(),
        itc_title: format!("```\nINTERCHOUETTE {n}\n```", n = itc.len()),
        itc_items: itc
            .into_iter()
            .map(|it| format_digest_item(it, &rec.digest_id))
            .collect(),
    }
}

/// Flat list for DM / fallback dump (overview + all threads' lines).
#[must_use]
pub fn digest_slack_messages(rec: &DigestRecord) -> Vec<String> {
    let post = digest_slack_post(rec);
    let mut out = Vec::new();
    out.push(post.overview);
    if !post.press_items.is_empty() {
        out.push(post.press_title);
        out.extend(with_item_grey_bars(&post.press_items));
    }
    if !post.for_you_items.is_empty() {
        out.push(post.for_you_title);
        out.extend(with_item_grey_bars(&post.for_you_items));
    }
    if !post.following_items.is_empty() {
        out.push(post.following_title);
        out.extend(with_item_grey_bars(&post.following_items));
    }
    if !post.twitter_items.is_empty() {
        out.push(post.twitter_title);
        out.extend(with_item_grey_bars(&post.twitter_items));
    }
    if !post.itc_items.is_empty() {
        out.push(post.itc_title);
        out.extend(with_item_grey_bars(&post.itc_items));
    }
    out
}

/// Slack / operator text for a digest (single blob).
#[must_use]
pub fn format_digest_slack(rec: &DigestRecord) -> String {
    digest_slack_messages(rec).join("\n\n")
}

/// Ship notice after mock/live Post.
#[must_use]
pub fn format_ship_notice(post_id: &str, detail: &str) -> String {
    format!("*Ship notice* `{post_id}`\n{detail}")
}

/// Playground `LinkedIn` ship: operator must paste manually; include the fenced body.
#[must_use]
pub fn format_playground_linkedin_ship_notice(
    post_id: &str,
    paste_block: &str,
    detail: &str,
) -> String {
    format!(
        ":white_check_mark: *Playground ship* `{post_id}`\n\
Fork BAT merged; status **published**. Paste on company Page:\n\n{paste_block}\n\n{detail}"
    )
}

/// Second Slack link: org **`drafts`** PR from `/accept` (manual Approve on org).
#[must_use]
pub fn format_org_draft_pr_notice(draft_id: &str, pr_url: &str) -> String {
    format!("*Org drafts PR* `{draft_id}`\nApprove on Interchouette-ITC `drafts`:\n{pr_url}")
}

/// Slack after mock/live ship failed (promote still stands).
#[must_use]
pub fn format_ship_fail(post_id: &str, error: &str) -> String {
    format!(
        "*Ship failed* `{post_id}`\n{}",
        clarify_bat_or_ship_error(error)
    )
}

/// Slack when Approve wake could not finish promote + ship.
#[must_use]
pub fn format_bat_fail(pr_number: u64, error: &str) -> String {
    format!(
        "*BAT failed* PR #{pr_number}\n{}\n\n:repeat: /retry_bat <DRAFT-…|POST-…|TWEET-…|XPOST-…>",
        clarify_bat_or_ship_error(error)
    )
}

/// Drop Playwright dumps; keep a short operator reason (incl. org `drafts` protection).
#[must_use]
pub fn shorten_ship_error(error: &str) -> String {
    clarify_bat_or_ship_error(error)
}

/// Operator-facing BAT / ship failure reason.
#[must_use]
pub fn clarify_bat_or_ship_error(error: &str) -> String {
    let e = error.trim();
    let low = e.to_ascii_lowercase();
    if crate::bat::github::contents_put_blocked_by_branch_protection(e)
        || e.contains("Contents API blocked - branch protection")
        || (low.contains("changes must be made through a pull request") && low.contains("protect"))
    {
        return "Org `drafts` (or `drafts_tweet`) rejects Contents API puts because branch protection requires a PR. \
Unprotect that staging branch on Interchouette-ITC/itcy-publications (keep `posts`/`tweets` protected). \
Ship did not run."
            .into();
    }
    if crate::bat::github::contents_put_sha_retryable(e)
        || e.contains("Contents API SHA race")
        || e.contains("Contents API SHA conflict")
    {
        return "Org `drafts` mirror hit a concurrent Contents update (SHA race). \
If POST is already on fork `posts`, `/retry_bat` re-syncs and ships. Ship did not run on this wake."
            .into();
    }
    if low.contains("aria-disabled")
        || (low.contains("tweetbutton") && low.contains("not enabled"))
        || low.contains("element is not enabled")
    {
        return "X Post button stayed disabled. Composer would not enable Post (text too long, or X blocked send). Promote still stands.".into();
    }
    if low.contains("browser has been closed")
        || low.contains("target page, context or browser has been closed")
    {
        return "Brave died mid root→reply (CDP browser.close or port steal). First pass never finished the overflow reply.".into();
    }
    if low.contains("already said that") {
        return "X rejected the Post as a duplicate of text already on the timeline (often a second submit of the same root). Overflow reply may be missing. Promote still stands.".into();
    }
    if let Some(i) = e.find("Call log:") {
        return clip_reason(e[..i].trim(), 400);
    }
    clip_reason(e, 400)
}

fn clip_reason(s: &str, max: usize) -> String {
    let compact = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max {
        return compact;
    }
    let mut out = String::new();
    for ch in compact.chars() {
        if out.chars().count() + 4 > max {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

/// Builds candidates from live hubs + Twitter, stores DIGEST, returns record.
///
/// # Errors
///
/// Returns [`DigestError`] when persistence fails (lane fetch failures are skipped).
pub async fn build_daily_digest(db_path: &Path) -> Result<DigestRecord, DigestError> {
    let seen = load_prior_day_seen_keys(db_path)?;
    let mut candidates: Vec<Candidate> = Vec::new();
    candidates.extend(collect_live_site_candidates(&seen).await);
    candidates.extend(collect_twitter_candidates().await);
    candidates.extend(collect_itc_lane_candidates().await);

    candidates.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.title.cmp(&b.title)));
    dedupe_twitter_near_body(&mut candidates);
    dedupe_twitter_search_authors(&mut candidates);
    dedupe_candidates(&mut candidates);
    let excluded = filter_prior_day_seen(&mut candidates, &seen);
    info!(
        excluded,
        seen_keys = seen.len(),
        "digest: prior-day freshness filter"
    );

    let lanes = partition_lanes(candidates);
    let press_pool = fair_press_by_host(lanes.live, PRESS_POOL, MAX_PER_HUB);
    let mut for_you = lanes.for_you;
    let mut following = lanes.following;
    let tweets = fair_tweets_by_query(lanes.tweets, MAX_TWITTER_ITEMS, MAX_PER_SEARCH_QUERY);
    let mut itc = lanes.itc;
    for_you.truncate(MAX_FOR_YOU_ITEMS);
    following.truncate(MAX_FOLLOWING_ITEMS);
    itc.truncate(MAX_ITC_ITEMS);

    let mut press_items: Vec<DigestItem> = press_pool
        .into_iter()
        .enumerate()
        .map(|(i, c)| candidate_to_item(i, c))
        .collect();
    fill_press_blurbs(&mut press_items).await;
    drop_listing_press_items(&mut press_items);
    press_items.truncate(MAX_SITE_ITEMS);
    warn_short_lane("press", press_items.len(), MAX_SITE_ITEMS);
    warn_short_lane("for_you", for_you.len(), MAX_FOR_YOU_ITEMS);
    warn_short_lane("following", following.len(), MAX_FOLLOWING_ITEMS);
    warn_short_lane("twitter", tweets.len(), MAX_TWITTER_ITEMS);
    info!(
        press = press_items.len(),
        for_you = for_you.len(),
        following = following.len(),
        twitter = tweets.len(),
        itc = itc.len(),
        "digest: lane sizes after press listing filter"
    );

    let mut items = press_items;
    for c in for_you
        .into_iter()
        .chain(following)
        .chain(tweets)
        .chain(itc)
    {
        items.push(candidate_to_item(items.len(), c));
    }
    for (i, it) in items.iter_mut().enumerate() {
        it.idx = i32::try_from(i + 1).unwrap_or(1);
    }
    if items.is_empty() {
        return Err(DigestError::Other(
            "no digest candidates (check live_sites.toml and twitter)".into(),
        ));
    }
    let digest_id = next_digest_id(db_path)?;
    insert_digest(db_path, &digest_id, &items)?;
    get_digest(db_path, &digest_id)?.ok_or_else(|| DigestError::Other("insert missing".into()))
}

fn candidate_to_item(i: usize, c: Candidate) -> DigestItem {
    DigestItem {
        idx: i32::try_from(i + 1).unwrap_or(1),
        title: c.title,
        url: c.url,
        subject: c.subject,
        lane: c.lane,
        weight: c.weight,
        detail: c.detail,
    }
}

struct LaneBuckets {
    live: Vec<Candidate>,
    for_you: Vec<Candidate>,
    following: Vec<Candidate>,
    tweets: Vec<Candidate>,
    itc: Vec<Candidate>,
}

fn partition_lanes(candidates: Vec<Candidate>) -> LaneBuckets {
    let mut live = Vec::new();
    let mut for_you = Vec::new();
    let mut following = Vec::new();
    let mut tweets = Vec::new();
    let mut itc = Vec::new();
    for c in candidates {
        if c.url
            .as_deref()
            .is_some_and(crate::sources::url_hygiene::is_linkedin_host)
        {
            continue;
        }
        match c.lane.as_str() {
            "for_you" => for_you.push(c),
            "following" => following.push(c),
            "twitter" => tweets.push(c),
            "itc_draft" | "itc_tweet" => itc.push(c),
            _ => live.push(c),
        }
    }
    LaneBuckets {
        live,
        for_you,
        following,
        tweets,
        itc,
    }
}

async fn collect_live_site_candidates(seen: &HashSet<String>) -> Vec<Candidate> {
    let sites = match load_live_sites() {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "digest: live_sites load failed");
            return Vec::new();
        }
    };
    let fetcher = HttpThenPublicPlaywright::new();
    let mut out = Vec::new();
    for site in sites {
        match fetch_hub_candidates(&fetcher, &site, seen).await {
            Ok(mut batch) => out.append(&mut batch),
            Err(e) => warn!(url = %site.url, error = %e, "digest: hub fetch failed"),
        }
    }
    out
}

async fn fetch_hub_candidates(
    fetcher: &HttpThenPublicPlaywright,
    site: &LiveSite,
    seen: &HashSet<String>,
) -> Result<Vec<Candidate>, String> {
    use crate::sources::ingest::PageFetcher;
    let html = fetcher
        .fetch_html(&site.url)
        .await
        .map_err(|e| e.to_string())?;
    let links = extract_article_links(&html, &site.url);
    let links = select_unseen_hub_links(links, seen, HUB_CANDIDATES);
    let mut out = Vec::new();
    for (url, title) in links {
        let subject = crate::sources::html::infer_subject(&title, "");
        out.push(Candidate {
            title: title.clone(),
            url: Some(url),
            subject: if subject.is_empty() { title } else { subject },
            lane: "live_site".into(),
            weight: site.weight,
            detail: String::new(),
            query: String::new(),
        });
    }
    Ok(out)
}

async fn fill_press_blurbs(items: &mut [DigestItem]) {
    for it in items.iter_mut() {
        if !is_x_lane(&it.lane) {
            it.detail.clear();
        }
    }
    let fetcher = Arc::new(HttpPageFetcher::new());
    let mut handles = Vec::new();
    for (i, it) in items.iter().enumerate() {
        if is_x_lane(&it.lane) {
            continue;
        }
        let Some(url) = it.url.clone().filter(|u| !u.is_empty()) else {
            continue;
        };
        let fetcher = Arc::clone(&fetcher);
        handles.push(tokio::spawn(async move {
            let html = tokio::time::timeout(Duration::from_secs(12), fetcher.fetch_html(&url))
                .await
                .ok()
                .and_then(Result::ok);
            let blurb = html
                .as_deref()
                .and_then(crate::sources::html::article_blurb);
            (i, blurb)
        }));
    }
    let mut filled = 0usize;
    for h in handles {
        match h.await {
            Ok((i, Some(b))) => {
                items[i].detail = b;
                filled += 1;
            }
            Ok((_, None)) => {}
            Err(e) => warn!(error = %e, "digest: press blurb task failed"),
        }
    }
    info!(filled, "digest: press article blurbs");
}

fn extract_article_links(body: &str, hub_url: &str) -> Vec<(String, String)> {
    let mut out = extract_newsletter_archive_links(body).unwrap_or_else(|| {
        if looks_like_syndication_feed(body) {
            extract_feed_article_links(body, hub_url)
        } else {
            extract_html_article_links(body, hub_url)
        }
    });
    out.sort_by(|a, b| {
        press_prefer_score(&b.0, &b.1)
            .cmp(&press_prefer_score(&a.0, &a.1))
            .then_with(|| a.1.cmp(&b.1))
    });
    out.truncate(HUB_LINK_SCAN_MAX);
    out
}

/// Rustler-style weekly archive JSON: curated off-host article links.
fn extract_newsletter_archive_links(body: &str) -> Option<Vec<(String, String)>> {
    let trimmed = body.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let newsletters = v.get("newsletters")?.as_array()?;
    let mut out = Vec::new();
    for nl in newsletters {
        let Some(articles) = nl.get("articles").and_then(|a| a.as_array()) else {
            continue;
        };
        for art in articles {
            let title = art
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .trim();
            let url = art.get("url").and_then(|u| u.as_str()).unwrap_or("").trim();
            if title.chars().count() < 12 {
                continue;
            }
            let abs = canonicalize_article_url(url);
            if !accept_curated_press_url(&abs) {
                continue;
            }
            if is_listing_seo_copy(title, "") {
                continue;
            }
            if out.iter().any(|(u, _)| u == &abs) {
                continue;
            }
            out.push((abs, title.to_string()));
        }
    }
    Some(out)
}

fn accept_curated_press_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return false;
    }
    if crate::sources::url_hygiene::is_linkedin_host(url) {
        return false;
    }
    !is_non_article_asset(&lower)
}

fn looks_like_syndication_feed(body: &str) -> bool {
    let head = body.get(..800).unwrap_or(body).to_ascii_lowercase();
    head.contains("<rss") || head.contains("<feed") || head.contains("<rdf:rdf")
}

fn extract_feed_article_links(xml: &str, hub_url: &str) -> Vec<(String, String)> {
    let base_host = host_of(hub_url).unwrap_or_default();
    let mut out = Vec::new();
    for block in feed_entry_blocks(xml) {
        let Some(url) = feed_entry_link(block) else {
            continue;
        };
        let Some(abs) = absolutize(hub_url, &url).map(|u| canonicalize_article_url(&u)) else {
            continue;
        };
        if !accept_press_candidate_url(&abs, &base_host) {
            continue;
        }
        let title = feed_entry_title(block)
            .filter(|t| t.chars().count() >= 12)
            .unwrap_or_else(|| path_title(&abs));
        if title.chars().count() < 12 || is_listing_seo_copy(&title, "") {
            continue;
        }
        if out.iter().any(|(u, _)| u == &abs) {
            continue;
        }
        out.push((abs, title));
    }
    out
}

fn feed_entry_blocks(xml: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for (open, close) in [("<item", "</item>"), ("<entry", "</entry>")] {
        let lower = xml.to_ascii_lowercase();
        let mut from = 0;
        while let Some(rel) = lower[from..].find(open) {
            let start = from + rel;
            let Some(gt) = xml[start..].find('>') else {
                break;
            };
            let body_start = start + gt + 1;
            let Some(rel_end) = lower[body_start..].find(close) else {
                break;
            };
            let end = body_start + rel_end;
            out.push(&xml[body_start..end]);
            from = end + close.len();
        }
    }
    out
}

fn feed_entry_link(block: &str) -> Option<String> {
    let lower = block.to_ascii_lowercase();
    // Atom: <link href="…" rel="alternate"/>
    let mut from = 0;
    while let Some(rel) = lower[from..].find("<link") {
        let start = from + rel;
        let Some(gt) = block[start..].find('>') else {
            break;
        };
        let tag = &block[start..=start + gt];
        let tag_l = tag.to_ascii_lowercase();
        from = start + gt + 1;
        if tag_l.contains("rel=\"self\"") || tag_l.contains("rel='self'") {
            continue;
        }
        if let Some(href) = attr_value(tag, "href") {
            if href.starts_with("http://") || href.starts_with("https://") || href.starts_with('/')
            {
                return Some(href);
            }
        }
        // RSS 2.0: <link>https://…</link>
        if !tag_l.contains("href=") {
            let rest = &block[from..];
            let rest_l = rest.to_ascii_lowercase();
            if let Some(end) = rest_l.find("</link>") {
                let raw = rest[..end].trim();
                if !raw.is_empty() {
                    return Some(xml_text_decode(raw));
                }
            }
        }
    }
    None
}

fn feed_entry_title(block: &str) -> Option<String> {
    let lower = block.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let after = &block[start..];
    let gt = after.find('>')?;
    let rest = &after[gt + 1..];
    let end = rest.to_ascii_lowercase().find("</title>")?;
    let raw = rest[..end].trim();
    let t = xml_text_decode(raw);
    (!t.is_empty()).then_some(t)
}

fn attr_value(tag: &str, name: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let needle = format!("{name}={quote}");
        if let Some(i) = tag.to_ascii_lowercase().find(&needle) {
            let rest = &tag[i + needle.len()..];
            if let Some(end) = rest.find(quote) {
                return Some(rest[..end].trim().to_string());
            }
        }
    }
    None
}

fn xml_text_decode(raw: &str) -> String {
    let t = raw
        .trim()
        .trim_start_matches("<![CDATA[")
        .trim_end_matches("]]>")
        .trim();
    t.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace('\n', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_html_article_links(html: &str, hub_url: &str) -> Vec<(String, String)> {
    let base_host = host_of(hub_url).unwrap_or_default();
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(idx) = rest.find("href=\"") {
        rest = &rest[idx + 6..];
        let Some(end) = rest.find('"') else { break };
        let href = rest[..end].trim();
        rest = &rest[end + 1..];
        let Some(abs) = absolutize(hub_url, href).map(|u| canonicalize_article_url(&u)) else {
            continue;
        };
        if !accept_press_candidate_url(&abs, &base_host) {
            continue;
        }
        let title = sniff_title_near(html, href).unwrap_or_else(|| path_title(&abs));
        if title.chars().count() < 12 {
            continue;
        }
        if is_listing_seo_copy(&title, "") {
            continue;
        }
        if out.iter().any(|(u, _)| u == &abs) {
            continue;
        }
        out.push((abs, title));
    }
    out
}

fn accept_press_candidate_url(url: &str, base_host: &str) -> bool {
    if host_of(url).as_deref() != Some(base_host) {
        return false;
    }
    if crate::sources::url_hygiene::is_linkedin_host(url) {
        return false;
    }
    looks_like_article_path(url)
}

/// Drop tracking query / fragment so feed UTMs do not fail the article path gate.
fn canonicalize_article_url(url: &str) -> String {
    let no_frag = url.split_once('#').map_or(url, |(a, _)| a);
    no_frag
        .split_once('?')
        .map_or(no_frag, |(a, _)| a)
        .trim_end_matches('/')
        .to_string()
}

/// Prefer deep article slugs; demote shallow /news/ section tails.
fn press_prefer_score(url: &str, title: &str) -> i32 {
    let lower = url.to_ascii_lowercase();
    let mut s = 0i32;
    if lower.contains("/article/")
        || lower.contains("/opinion/")
        || lower.contains("/feature/")
        || lower.contains("/story/")
        || lower.contains("/blog/")
    {
        s += 40;
    }
    if has_dated_path(&lower) {
        s += 30;
    }
    if has_numeric_id_article_path(&lower) {
        s += 45;
    }
    if let Some(slug) = path_last_segment(&lower) {
        if is_deep_article_slug(slug) {
            s += 35;
        } else if slug.contains('-') && slug.len() >= 16 {
            s += 10;
        } else if !slug.contains('-') {
            s -= 40;
        }
        if is_shallow_section_slug(slug) {
            s -= 60;
        }
    }
    if is_listing_seo_copy(title, "") {
        s -= 80;
    }
    s
}

fn looks_like_article_path(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if is_non_article_asset(&lower) || is_listing_path(&lower) {
        return false;
    }
    has_article_shape(&lower)
}

fn is_non_article_asset(lower: &str) -> bool {
    lower.contains("/video/")
        || lower.contains("/a/img/")
        || lower.contains("/a/neutron/")
        || lower.contains("/fonts/")
        || lower.contains("/wp-content/")
        || lower.contains("/cdn-cgi/")
        || lower.contains(".jpg")
        || lower.contains(".jpeg")
        || lower.contains(".png")
        || lower.contains(".gif")
        || lower.contains(".webp")
        || lower.contains(".svg")
        || lower.contains(".woff")
        || lower.contains(".css")
        || lower.contains(".js")
        || lower.contains('?')
}

fn is_listing_path(lower: &str) -> bool {
    if lower.contains("/category/")
        || lower.contains("/section/")
        || lower.contains("/tag/")
        || lower.contains("/topic/")
        || lower.contains("/topics/")
        || lower.contains("/author/")
        || lower.contains("/authors/")
        || lower.contains("/page/")
        || lower.contains("/avis/")
        || lower.contains("/entreprise/")
    {
        return true;
    }
    let Some(slug) = path_last_segment(lower) else {
        return true;
    };
    if is_known_listing_slug(slug) {
        return true;
    }
    if has_dated_path(lower) || lower.contains("/this-week") {
        return false;
    }
    // Hub section tails: /news/foo, /news/artificial-intelligence (not a story).
    if (lower.contains("/news/")
        || lower.contains("/blog/")
        || lower.contains("/blogs/")
        || lower.contains("/markets/")
        || lower.contains("/business/")
        || lower.contains("/policy/"))
        && is_shallow_section_slug(slug)
    {
        return true;
    }
    false
}

fn is_known_listing_slug(slug: &str) -> bool {
    matches!(
        slug,
        "news"
            | "blog"
            | "blogs"
            | "posts"
            | "stories"
            | "markets"
            | "markets-finance"
            | "company-announcements"
            | "cryptocurrencies"
            | "coins"
            | "latest"
            | "updates"
            | "press"
            | "press-releases"
            | "announcements"
            | "category"
            | "categories"
            | "archive"
            | "archives"
            | "index"
            | "home"
            | "all"
            | "feed"
            | "rss"
    )
}

/// Topic / channel slug: few words, no digits (vs long news headlines).
fn is_shallow_section_slug(slug: &str) -> bool {
    let words = slug.split('-').filter(|p| !p.is_empty()).count();
    let has_digit = slug.chars().any(|c| c.is_ascii_digit());
    if has_digit {
        return false;
    }
    if !slug.contains('-') {
        return slug.len() < 28;
    }
    words <= 3 && slug.len() < 40
}

fn has_dated_path(lower: &str) -> bool {
    lower.contains("/2024/") || lower.contains("/2025/") || lower.contains("/2026/")
}

fn has_article_shape(lower: &str) -> bool {
    if has_dated_path(lower) {
        return true;
    }
    if lower.contains("/jobs/") || lower.contains("/this-week") {
        return true;
    }
    let Some(slug) = path_last_segment(lower) else {
        return false;
    };
    // decrypt.co/294851/long-article-slug (numeric id, no /news/ marker).
    if has_numeric_id_article_path(lower) {
        return is_deep_article_slug(slug);
    }
    let strong = lower.contains("/article/")
        || lower.contains("/opinion/")
        || lower.contains("/feature/")
        || lower.contains("/story/")
        || lower.contains("/post/");
    let weak = lower.contains("/news/")
        || lower.contains("/blog/")
        || lower.contains("/blogs/")
        || lower.contains("/posts/")
        || lower.contains("/markets/")
        || lower.contains("/business/")
        || lower.contains("/policy/");
    if strong {
        return slug.contains('-') && slug.len() >= 16;
    }
    if weak {
        return is_deep_article_slug(slug);
    }
    // WordPress-style root permalinks: /morgan-stanley-data-shows-blackrock/
    is_deep_article_slug(slug)
}

fn has_numeric_id_article_path(lower: &str) -> bool {
    let path = lower
        .split("://")
        .nth(1)
        .unwrap_or(lower)
        .split_once('/')
        .map_or("", |(_, p)| p);
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    segs.len() >= 2 && segs[0].len() >= 4 && segs[0].chars().all(|c| c.is_ascii_digit())
}

fn is_deep_article_slug(slug: &str) -> bool {
    if is_shallow_section_slug(slug) {
        return false;
    }
    let words = slug.split('-').filter(|p| !p.is_empty()).count();
    let has_digit = slug.chars().any(|c| c.is_ascii_digit());
    if words >= 5 {
        return true;
    }
    if words >= 4 && slug.len() >= 24 {
        return true;
    }
    has_digit && words >= 3 && slug.len() >= 16
}

fn path_last_segment(url: &str) -> Option<&str> {
    let path = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split_once('/')
        .map_or("", |(_, p)| p);
    let seg = path.trim_matches('/').rsplit('/').next().unwrap_or("");
    if seg.is_empty() {
        None
    } else {
        Some(seg)
    }
}

fn is_listing_seo_copy(title: &str, detail: &str) -> bool {
    let blob = format!("{title} {detail}").to_ascii_lowercase();
    let needles = [
        "discover the latest",
        "stay up to speed",
        "stay informed with real-time",
        "latest stories, updates, and insights",
        "latest news & updates",
        "latest ai news",
        "news, features, and analysis",
        "features, and analysis",
        "from generative to",
        "recent news",
        "newsroom | recent",
        "the projects, currencies, and events shaping",
        "browse all",
        "all the latest",
        "the latest news",
        "the latest ai",
    ];
    needles.iter().any(|n| blob.contains(n))
        || (blob.contains("latest")
            && blob.contains("news")
            && (blob.contains("updates") || blob.contains("features") || blob.contains("analysis"))
            && blob.len() < 220)
}

fn drop_listing_press_items(items: &mut Vec<DigestItem>) {
    items.retain(|it| {
        if is_x_lane(&it.lane) || is_itc_lane(&it.lane) {
            return true;
        }
        if it
            .url
            .as_deref()
            .is_some_and(|u| !looks_like_article_path(u))
        {
            return false;
        }
        !is_listing_seo_copy(&it.title, &it.detail)
    });
}

fn sniff_title_near(html: &str, href: &str) -> Option<String> {
    let pos = html.find(href)?;
    let window = &html[pos..html.len().min(pos + 400)];
    let start = window.find('>')?;
    let after = &window[start + 1..];
    let end = after.find('<')?;
    let t = after[..end]
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .trim()
        .to_string();
    (t.chars().count() >= 12).then_some(t)
}

fn path_title(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .replace(['-', '_'], " ")
}

fn host_of(url: &str) -> Option<String> {
    let u = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = u.split('/').next()?.trim().to_ascii_lowercase();
    Some(host.strip_prefix("www.").unwrap_or(&host).to_string())
}

fn absolutize(hub: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.starts_with("https://") || href.starts_with("http://") {
        return Some(href.to_string());
    }
    if href.starts_with("//") {
        return Some(format!("https:{href}"));
    }
    if href.starts_with('#') || href.starts_with("javascript:") || href.starts_with("mailto:") {
        return None;
    }
    if href.starts_with('/') {
        let host = host_of(hub)?;
        let scheme = if hub.starts_with("http://") {
            "http"
        } else {
            "https"
        };
        return Some(format!("{scheme}://{host}{href}"));
    }
    None
}

async fn collect_twitter_candidates() -> Vec<Candidate> {
    let tool = match TwitterTool::from_disk() {
        Ok(t) => t,
        Err(e) => {
            info!(error = %e, "digest: twitter tool skipped");
            return Vec::new();
        }
    };
    info!("{}", tool.creds().status_line());
    let pool = match crate::sources::load_twitter_query_pool() {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "digest: twitter_queries.toml failed; using empty pool");
            crate::sources::TwitterQueryPool {
                queries: Vec::new(),
                excludes: Vec::new(),
            }
        }
    };
    let plan = crate::sources::plan_twitter_searches_from_pool(&pool);
    let weight_by_q: std::collections::HashMap<String, i32> = plan
        .searches
        .iter()
        .map(|s| (s.q.clone(), s.weight))
        .collect();
    let search_strings: Vec<String> = plan.searches.iter().map(|s| s.q.clone()).collect();
    let log_queries: Vec<String> = search_strings
        .iter()
        .map(|q| crate::sources::query_for_log(q))
        .collect();
    info!(
        pool = pool.queries.len(),
        searches = plan.searches.len(),
        queries = ?log_queries,
        "digest: twitter search plan (spaced sample)"
    );
    let mut out = Vec::new();
    match tool.digest_pulse(&search_strings).await {
        Ok(hits) => {
            let for_you = hits.iter().filter(|h| h.lane == "for_you").count();
            let following = hits.iter().filter(|h| h.lane == "following").count();
            let searches = hits.iter().filter(|h| h.lane == "twitter").count();
            info!(
                hits = hits.len(),
                for_you,
                following,
                searches,
                planned = plan.searches.len(),
                "digest: twitter pulse (For you + Following then searches)"
            );
            for h in hits {
                let weight = if is_home_lane(&h.lane) {
                    8
                } else {
                    weight_for_hit(&h.query, &weight_by_q)
                };
                out.push(hit_to_candidate(h, weight));
            }
        }
        Err(e) => warn!(error = %e, "digest: twitter pulse failed"),
    }
    out
}

fn weight_for_hit(query: &str, map: &std::collections::HashMap<String, i32>) -> i32 {
    let q = query.trim();
    if q.is_empty() {
        return 9;
    }
    if let Some(w) = map.get(q) {
        return *w;
    }
    // Pulse may tag without lang:en / excludes; match by prefix.
    for (k, w) in map {
        if q.starts_with(k.as_str()) || k.starts_with(q) {
            return *w;
        }
    }
    9
}

/// Up to press / dual home / Twitter search / Interchouette portfolio slots.
#[cfg(test)]
fn take_lane_mix(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let lanes = partition_lanes(candidates);
    let mut press = fair_press_by_host(lanes.live, MAX_SITE_ITEMS, MAX_PER_HUB);
    let mut for_you = lanes.for_you;
    let mut following = lanes.following;
    let tweets = fair_tweets_by_query(lanes.tweets, MAX_TWITTER_ITEMS, MAX_PER_SEARCH_QUERY);
    let mut itc = lanes.itc;
    for_you.truncate(MAX_FOR_YOU_ITEMS);
    following.truncate(MAX_FOLLOWING_ITEMS);
    itc.truncate(MAX_ITC_ITEMS);
    press.extend(for_you);
    press.extend(following);
    press.extend(tweets);
    press.extend(itc);
    press
}

async fn collect_itc_lane_candidates() -> Vec<Candidate> {
    crate::sources::itc_digest::collect_itc_candidates()
        .await
        .into_iter()
        .map(|c| Candidate {
            title: c.title,
            url: c.url,
            subject: c.subject,
            lane: c.lane,
            weight: c.weight,
            detail: c.detail,
            query: String::new(),
        })
        .collect()
}

/// Round-robin across search queries so one keyword cannot fill TWITTER 20.
fn fair_tweets_by_query(
    mut items: Vec<Candidate>,
    max: usize,
    max_per_query: usize,
) -> Vec<Candidate> {
    if items.is_empty() || max == 0 {
        return Vec::new();
    }
    items.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.title.cmp(&b.title)));
    let mut by_q: Vec<(String, Vec<Candidate>)> = Vec::new();
    for c in items {
        let key = if c.query.trim().is_empty() {
            "_unscoped".into()
        } else {
            c.query.clone()
        };
        if let Some((_, bucket)) = by_q.iter_mut().find(|(h, _)| h == &key) {
            bucket.push(c);
        } else {
            by_q.push((key, vec![c]));
        }
    }
    by_q.sort_by(|a, b| {
        let wa = a.1.first().map_or(0, |c| c.weight);
        let wb = b.1.first().map_or(0, |c| c.weight);
        wb.cmp(&wa).then_with(|| a.0.cmp(&b.0))
    });
    let mut out = Vec::new();
    let mut taken: Vec<usize> = vec![0; by_q.len()];
    loop {
        let mut progressed = false;
        for (i, (_, bucket)) in by_q.iter().enumerate() {
            if out.len() >= max {
                break;
            }
            if taken[i] >= max_per_query || taken[i] >= bucket.len() {
                continue;
            }
            out.push(bucket[taken[i]].clone());
            taken[i] += 1;
            progressed = true;
        }
        if !progressed || out.len() >= max {
            break;
        }
    }
    while out.len() < max {
        let mut progressed = false;
        for (i, (_, bucket)) in by_q.iter().enumerate() {
            if out.len() >= max {
                break;
            }
            if taken[i] >= bucket.len() {
                continue;
            }
            out.push(bucket[taken[i]].clone());
            taken[i] += 1;
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
    out
}

/// Round-robin across hosts so one hub (e.g. `InfoWorld`) cannot fill the press slots.
fn fair_press_by_host(mut items: Vec<Candidate>, max: usize, max_per_hub: usize) -> Vec<Candidate> {
    if items.is_empty() || max == 0 {
        return Vec::new();
    }
    items.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.title.cmp(&b.title)));
    let mut by_host: Vec<(String, Vec<Candidate>)> = Vec::new();
    for c in items {
        let host = c
            .url
            .as_deref()
            .and_then(host_of)
            .unwrap_or_else(|| c.lane.clone());
        if let Some((_, bucket)) = by_host.iter_mut().find(|(h, _)| h == &host) {
            bucket.push(c);
        } else {
            by_host.push((host, vec![c]));
        }
    }
    by_host.sort_by(|a, b| {
        let wa = a.1.first().map_or(0, |c| c.weight);
        let wb = b.1.first().map_or(0, |c| c.weight);
        wb.cmp(&wa).then_with(|| a.0.cmp(&b.0))
    });
    let mut out = Vec::new();
    let mut taken: Vec<usize> = vec![0; by_host.len()];
    loop {
        let mut progressed = false;
        for (i, (_, bucket)) in by_host.iter().enumerate() {
            if out.len() >= max {
                break;
            }
            if taken[i] >= max_per_hub || taken[i] >= bucket.len() {
                continue;
            }
            out.push(bucket[taken[i]].clone());
            taken[i] += 1;
            progressed = true;
        }
        if !progressed || out.len() >= max {
            break;
        }
    }
    // Second pass: keep round-robin without per-hub cap to fill toward `max`.
    while out.len() < max {
        let mut progressed = false;
        for (i, (_, bucket)) in by_host.iter().enumerate() {
            if out.len() >= max {
                break;
            }
            if taken[i] >= bucket.len() {
                continue;
            }
            out.push(bucket[taken[i]].clone());
            taken[i] += 1;
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
    out
}

fn hit_to_candidate(h: TwitterHit, weight: i32) -> Candidate {
    let tweet = tweet_text_from_hit(&h);
    let lane = match h.lane.as_str() {
        "following" => "following".into(),
        "for_you" => "for_you".into(),
        _ => "twitter".into(),
    };
    Candidate {
        title: h.title.clone(),
        url: Some(h.url),
        subject: h.subject,
        lane,
        weight,
        detail: tweet,
        query: h.query,
    }
}

fn tweet_text_from_hit(h: &TwitterHit) -> String {
    let d = h.detail.trim();
    if !d.is_empty() && !is_twitter_lane_hint(d) {
        return d.to_string();
    }
    if let Some((_, rest)) = h.title.split_once(" · ") {
        let rest = rest.trim();
        if !rest.is_empty() {
            return rest.to_string();
        }
    }
    h.title.clone()
}

/// Near-duplicate tweet bodies (cross-status spam clones).
fn dedupe_twitter_near_body(items: &mut Vec<Candidate>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|c| {
        if !is_x_lane(&c.lane) {
            return true;
        }
        let fp = tweet_body_fingerprint(&c.detail);
        if fp.len() < 40 {
            return true;
        }
        seen.insert(fp)
    });
}

fn tweet_body_fingerprint(detail: &str) -> String {
    let lower = detail.to_ascii_lowercase();
    let mut clean = String::new();
    for token in lower.split_whitespace() {
        if token.starts_with('@') || token.starts_with("http") {
            continue;
        }
        let t = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
        if t.is_empty() {
            continue;
        }
        if !clean.is_empty() {
            clean.push(' ');
        }
        clean.push_str(t);
    }
    clean
}

/// Keep at most [`MAX_SEARCH_PER_AUTHOR`] search-lane hits per handle (not following).
fn dedupe_twitter_search_authors(items: &mut Vec<Candidate>) {
    let mut counts = std::collections::HashMap::<String, usize>::new();
    items.retain(|c| {
        if c.lane != "twitter" {
            return true;
        }
        let Some(k) = twitter_author_key(c) else {
            return true;
        };
        let n = counts.entry(k).or_insert(0);
        if *n >= MAX_SEARCH_PER_AUTHOR {
            return false;
        }
        *n += 1;
        true
    });
}

fn twitter_author_key(c: &Candidate) -> Option<String> {
    if let Some(u) = c.url.as_deref() {
        if let Some(h) = handle_from_status_url(u) {
            return Some(h);
        }
    }
    twitter_author_key_from_title(&c.title)
}

fn handle_from_status_url(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://x.com/")
        .or_else(|| lower.strip_prefix("http://x.com/"))
        .or_else(|| lower.strip_prefix("https://twitter.com/"))
        .or_else(|| lower.strip_prefix("http://twitter.com/"))?;
    let handle = rest.split('/').next()?.trim();
    if handle.len() < 2 || handle == "i" || handle == "intent" {
        return None;
    }
    Some(handle.to_string())
}

fn twitter_author_key_from_title(title: &str) -> Option<String> {
    title.split_whitespace().find_map(|part| {
        let p = part.trim_matches(|c: char| matches!(c, '·' | ':' | ',' | ';' | ')' | '('));
        let rest = p.strip_prefix('@')?;
        let rest = rest.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
        if rest.len() >= 2 {
            Some(rest.to_ascii_lowercase())
        } else {
            None
        }
    })
}

fn dedupe_candidates(items: &mut Vec<Candidate>) {
    let mut seen_url = std::collections::HashSet::new();
    let mut seen_title = std::collections::HashSet::new();
    items.retain(|c| {
        if let Some(u) = &c.url {
            if !seen_url.insert(u.clone()) {
                return false;
            }
        }
        let key = c.title.to_ascii_lowercase();
        seen_title.insert(key)
    });
}

/// `DIGEST-YYYYMMDD` lex bounds: include lookback day, exclude today onward.
fn digest_id_range_bounds(today: NaiveDate) -> (String, String) {
    let lookback = today
        .checked_sub_days(Days::new(SEEN_LOOKBACK_DAYS))
        .unwrap_or(today);
    (
        format!("DIGEST-{}", lookback.format("%Y%m%d")),
        format!("DIGEST-{}", today.format("%Y%m%d")),
    )
}

fn normalize_digest_url(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('/');
    let Some(scheme_end) = s.find("://") else {
        return s.to_ascii_lowercase();
    };
    let scheme = &s[..scheme_end];
    let after = &s[scheme_end + 3..];
    let (host, path) = after
        .find('/')
        .map_or((after, ""), |i| (&after[..i], &after[i..]));
    format!(
        "{}://{}{}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase(),
        path
    )
}

fn normalize_digest_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn digest_item_seen_key(url: Option<&str>, title: &str) -> String {
    if let Some(u) = url.map(str::trim).filter(|u| !u.is_empty()) {
        return format!("u:{}", normalize_digest_url(u));
    }
    format!("t:{}", normalize_digest_title(title))
}

fn candidate_seen_key(c: &Candidate) -> String {
    digest_item_seen_key(c.url.as_deref(), &c.title)
}

/// Loads URL/title keys from digests in `[today-7d, today)` (prior calendar days only).
///
/// # Errors
///
/// Returns [`DigestError`] on `SQLite` failure.
fn load_prior_day_seen_keys(db_path: &Path) -> Result<HashSet<String>, DigestError> {
    load_prior_day_seen_keys_on(db_path, Local::now().date_naive())
}

fn load_prior_day_seen_keys_on(
    db_path: &Path,
    today: NaiveDate,
) -> Result<HashSet<String>, DigestError> {
    ensure_digest_schema(db_path)?;
    let conn = open_configured(db_path)?;
    let (lower, upper) = digest_id_range_bounds(today);
    let mut stmt = conn.prepare(RECENT_ITEM_KEYS)?;
    let rows = stmt.query_map(params![lower, upper], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut set = HashSet::new();
    for row in rows {
        let (url, title) = row?;
        set.insert(digest_item_seen_key(url.as_deref(), &title));
    }
    Ok(set)
}

fn filter_prior_day_seen(items: &mut Vec<Candidate>, seen: &HashSet<String>) -> usize {
    let before = items.len();
    items.retain(|c| {
        // Portfolio cites are stable GitHub/site URLs; still show daily in INTERCHOUETTE.
        if is_itc_lane(&c.lane) {
            return true;
        }
        !seen.contains(&candidate_seen_key(c))
    });
    before.saturating_sub(items.len())
}

/// Keep up to `max_keep` hub links whose URL/title is not in the prior-day seen set.
fn select_unseen_hub_links(
    links: Vec<(String, String)>,
    seen: &HashSet<String>,
    max_keep: usize,
) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(max_keep.min(links.len()));
    for (url, title) in links {
        let key = digest_item_seen_key(Some(&url), &title);
        if seen.contains(&key) {
            continue;
        }
        out.push((url, title));
        if out.len() >= max_keep {
            break;
        }
    }
    out
}

fn warn_short_lane(lane: &str, got: usize, target: usize) {
    if got < target {
        warn!(
            lane,
            got, target, "digest: lane short after freshness filter"
        );
    }
}

/// Resolve item indices from a digest (1-based).
///
/// # Errors
///
/// Returns [`DigestError`] when an index is missing.
pub fn pick_items<'a>(
    rec: &'a DigestRecord,
    indices: &[i32],
) -> Result<Vec<&'a DigestItem>, DigestError> {
    let mut out = Vec::new();
    for idx in indices {
        let Some(it) = rec.items.iter().find(|i| i.idx == *idx) else {
            return Err(DigestError::Other(format!(
                "digest `{}` has no item {idx}",
                rec.digest_id
            )));
        };
        out.push(it);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ship_fail_slack_strips_playwright_dump() {
        let dump = "publish: Brave X post failed (exit Some(1)):  {\"ok\":false,\"detail\":\"locator.click: Timeout 30000ms exceeded.\\nCall log:\\n  - waiting for locator('[data-testid=\\\"tweetButton\\\"]')\\n    - locator resolved to <button disabled aria-disabled=\\\"true\\\" data-testid=\\\"tweetButton\\\">\\n  - attempting click action\\n    - element is not enabled\\n\"}";
        let out = format_ship_fail("XPOST-20260814-000010", dump);
        assert!(out.starts_with("*Ship failed* `XPOST-20260814-000010`"));
        assert!(out.contains("Post button stayed disabled"));
        assert!(!out.contains("Call log:"));
        assert!(out.len() < 400);
    }

    #[test]
    fn bat_fail_slack_names_org_drafts_protection() {
        // DRAFT-20260829-000132: Approve merged posts PR then Contents put 409; no Slack reason.
        let api = r#"github api: put 2026/08/29/DRAFT-20260829-000132/body.md: {"message":"Could not create file: Changes must be made through a pull request.","documentation_url":"https://docs.github.com/articles/about-protected-branches","status":"409"}"#;
        let out = format_bat_fail(63, api);
        assert!(out.starts_with("*BAT failed* PR #63"), "{out}");
        assert!(out.contains("branch protection"), "{out}");
        assert!(out.contains("Ship did not run"), "{out}");
        assert!(out.contains("/retry_bat"), "{out}");
        assert!(!out.contains("documentation_url"), "{out}");
    }

    #[test]
    fn bat_fail_slack_names_org_drafts_sha_conflict() {
        // DRAFT-20260901-000138: merge ok, org `drafts` mirror PUT stale SHA; playground ship via retry.
        let api = r#"github api: put 2026/09/01/DRAFT-20260901-000138/body.md: {"message":"is at 5fd8c11ecd6e55466742d8ea626b83915e28e5f2 but expected fb3383a4729939701700d5fd2c31928b45c16a8d","documentation_url":"https://docs.github.com/rest/repos/contents#create-or-update-file-contents","status":"409"}"#;
        let out = format_bat_fail(66, api);
        assert!(out.starts_with("*BAT failed* PR #66"), "{out}");
        assert!(out.contains("SHA race"), "{out}");
        assert!(out.contains("/retry_bat"), "{out}");
        assert!(!out.contains("documentation_url"), "{out}");
    }

    #[test]
    fn bat_fail_slack_names_org_drafts_missing_sha() {
        // DRAFT-20260901-000141: meta PUT create raced; POST merge ok, sync aborted BAT.
        let api = r#"github api: put 2026/09/01/DRAFT-20260901-000141/meta.toml: {"message":"Invalid request.\n\n\"sha\" wasn't supplied.","documentation_url":"https://docs.github.com/rest/repos/contents#create-or-update-file-contents","status":"422"}"#;
        let out = format_bat_fail(73, api);
        assert!(out.starts_with("*BAT failed* PR #73"), "{out}");
        assert!(out.contains("SHA race"), "{out}");
        assert!(out.contains("/retry_bat"), "{out}");
        assert!(!out.contains("documentation_url"), "{out}");
    }

    #[test]
    fn ship_fail_clarifies_already_said_that() {
        let e = r#"publish: Brave X post failed (exit Some(1)): {"ok":false,"detail":"X rejected Post: Whoops! You already said that. screenshot=pw/x.png"}"#;
        let out = format_ship_fail("XPOST-20260829-000094", e);
        assert!(out.contains("duplicate"), "{out}");
        assert!(out.contains("Overflow reply"), "{out}");
    }

    #[test]
    fn playground_linkedin_ship_notice_includes_paste_block() {
        let paste = ":clipboard: LinkedIn paste (copy the block only; playground = paste on company Page):\n```\nHello\n```";
        let out = format_playground_linkedin_ship_notice(
            "POST-20260831-000136",
            paste,
            "playground ship ok pubs_pr=#65",
        );
        assert!(out.contains("*Playground ship*"), "{out}");
        assert!(out.contains("**published**"), "{out}");
        assert!(out.contains(paste), "{out}");
        assert!(out.contains("pubs_pr=#65"), "{out}");
    }

    #[test]
    fn ship_notice_shape() {
        let out = format_ship_notice("XPOST-1", "https://x.com/Interchouette/status/1");
        assert_eq!(
            out,
            "*Ship notice* `XPOST-1`\nhttps://x.com/Interchouette/status/1"
        );
        let notice = format_org_draft_pr_notice(
            "DRAFT-20260801-000001",
            "https://github.com/Interchouette-ITC/itcy-publications/pull/70",
        );
        assert!(notice.contains("Org drafts PR"));
        assert!(notice.contains("pull/70"));
    }

    #[test]
    fn extract_links_same_host() {
        let html = r#"<a href="/article/foo-bar-baz-long">A very long enough title here</a>"#;
        let links = extract_article_links(html, "https://www.infoworld.com/");
        assert!(!links.is_empty());
        assert!(links[0].0.contains("infoworld.com"));
    }

    #[test]
    fn extract_newsletter_archive_keeps_offhost_articles() {
        let json = r#"{
  "pages": 1,
  "newsletters": [{
    "id": 14,
    "subject": "Rustler Weekly",
    "articles": [
      {
        "title": "Five rust-lang teams formally adopt an LLM usage policy",
        "url": "https://blog.rust-lang.org/inside-rust/2026/08/05/rust-langrust-is-adopting-an-llm-policy"
      },
      {
        "title": "GraphForge embedded graph database with Rust core",
        "url": "https://github.com/CurateLabs/graphforge"
      },
      {
        "title": "LinkedIn job that must be dropped",
        "url": "https://www.linkedin.com/jobs/view/1"
      }
    ]
  }]
}"#;
        let links = extract_article_links(
            json,
            "https://rustler.in/api/newsletter/archive?page=1&limit=12",
        );
        assert_eq!(links.len(), 2);
        assert!(links.iter().any(|(u, _)| u.contains("blog.rust-lang.org")));
        assert!(links.iter().any(|(u, _)| u.contains("github.com")));
        assert!(!links.iter().any(|(u, _)| u.contains("linkedin")));
    }

    #[test]
    fn extract_rss_feed_items_not_channel_link() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
<title>Decrypt</title>
<link>https://decrypt.co/feed</link>
<item>
  <title>Trump-Linked World Liberty Gets Conditional Bank Charter for USD1</title>
  <link>https://decrypt.co/375726/trump-world-liberty-bank-charter-usd1-stablecoin</link>
</item>
<item>
  <title>Topic hub must not win</title>
  <link>https://decrypt.co/news/artificial-intelligence</link>
</item>
</channel></rss>"#;
        let links = extract_article_links(xml, "https://decrypt.co/feed");
        assert_eq!(links.len(), 1);
        assert!(links[0]
            .0
            .contains("/375726/trump-world-liberty-bank-charter-usd1-stablecoin"));
        assert!(!links
            .iter()
            .any(|(u, _)| u.contains("artificial-intelligence")));
    }

    #[test]
    fn extract_atom_feed_strips_utm() {
        let xml = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
<entry>
  <title>Cloudflare Agent Tracing Arrives</title>
  <link href="https://www.infoq.com/news/2026/08/cloudflare-agent-tracing/?utm_source=infoq" rel="alternate"/>
</entry>
</feed>"#;
        let links = extract_article_links(xml, "https://www.infoq.com/feed/");
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].0,
            "https://www.infoq.com/news/2026/08/cloudflare-agent-tracing"
        );
        assert!(!links[0].0.contains('?'));
    }

    #[test]
    fn roundtrip_digest_store() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("d.db");
        let id = next_digest_id(&db).unwrap();
        assert!(id.starts_with("DIGEST-"));
        insert_digest(
            &db,
            &id,
            &[DigestItem {
                idx: 1,
                title: "T".into(),
                url: Some("https://example.com/a".into()),
                subject: "subj".into(),
                lane: "live_site".into(),
                weight: 10,
                detail: String::new(),
            }],
        )
        .unwrap();
        let got = get_digest(&db, &id).unwrap().unwrap();
        assert_eq!(got.items.len(), 1);
        assert_eq!(latest_open_digest(&db).unwrap().unwrap().digest_id, id);
    }

    fn sample_item(title: &str, url: Option<&str>) -> DigestItem {
        DigestItem {
            idx: 1,
            title: title.into(),
            url: url.map(str::to_string),
            subject: title.into(),
            lane: "live_site".into(),
            weight: 5,
            detail: String::new(),
        }
    }

    fn digest_id_days_ago(days_ago: u64, seq: u32) -> String {
        let day = Local::now()
            .date_naive()
            .checked_sub_days(Days::new(days_ago))
            .unwrap();
        format!("DIGEST-{}-{seq:06}", day.format("%Y%m%d"))
    }

    fn freshness_cand(title: &str, url: Option<&str>) -> Candidate {
        freshness_cand_lane(title, url, "live_site")
    }

    fn freshness_cand_lane(title: &str, url: Option<&str>, lane: &str) -> Candidate {
        Candidate {
            title: title.into(),
            url: url.map(str::to_string),
            subject: title.into(),
            lane: lane.into(),
            weight: 5,
            detail: String::new(),
            query: String::new(),
        }
    }

    #[test]
    fn prior_day_url_excluded() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("d.db");
        let id = digest_id_days_ago(1, 1);
        insert_digest(
            &db,
            &id,
            &[sample_item("Old story", Some("https://example.com/a"))],
        )
        .unwrap();
        let seen = load_prior_day_seen_keys(&db).unwrap();
        let mut items = vec![freshness_cand("Old story", Some("https://example.com/a"))];
        assert_eq!(filter_prior_day_seen(&mut items, &seen), 1);
        assert!(items.is_empty());
    }

    #[test]
    fn prior_day_seen_keeps_itc_portfolio_urls() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("d.db");
        let id = digest_id_days_ago(1, 1);
        let url = "https://github.com/Interchouette-ITC/evaluator";
        insert_digest(
            &db,
            &id,
            &[DigestItem {
                idx: 1,
                title: "DRAFT · evaluator".into(),
                url: Some(url.into()),
                subject: "evaluator".into(),
                lane: "itc_draft".into(),
                weight: 9,
                detail: "Public evaluator crate.".into(),
            }],
        )
        .unwrap();
        let seen = load_prior_day_seen_keys(&db).unwrap();
        assert!(seen.contains(&digest_item_seen_key(Some(url), "DRAFT · evaluator")));
        let mut items = vec![freshness_cand_lane(
            "DRAFT · evaluator",
            Some(url),
            "itc_draft",
        )];
        assert_eq!(filter_prior_day_seen(&mut items, &seen), 0);
        assert_eq!(items.len(), 1);
        let mut tweet = vec![freshness_cand_lane(
            "TWEET · tvscreener-rs",
            Some("https://github.com/Interchouette-ITC/tvscreener-rs"),
            "itc_tweet",
        )];
        assert_eq!(filter_prior_day_seen(&mut tweet, &seen), 0);
        assert_eq!(tweet.len(), 1);
    }

    #[test]
    fn same_day_url_not_excluded() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("d.db");
        let id = digest_id_days_ago(0, 1);
        insert_digest(
            &db,
            &id,
            &[sample_item(
                "Today story",
                Some("https://example.com/today"),
            )],
        )
        .unwrap();
        let seen = load_prior_day_seen_keys(&db).unwrap();
        let mut items = vec![freshness_cand(
            "Today story",
            Some("https://example.com/today"),
        )];
        assert_eq!(filter_prior_day_seen(&mut items, &seen), 0);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn outside_seven_day_window_kept() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("d.db");
        let id = digest_id_days_ago(8, 1);
        insert_digest(
            &db,
            &id,
            &[sample_item("Ancient", Some("https://example.com/old"))],
        )
        .unwrap();
        let seen = load_prior_day_seen_keys(&db).unwrap();
        let mut items = vec![freshness_cand("Ancient", Some("https://example.com/old"))];
        assert_eq!(filter_prior_day_seen(&mut items, &seen), 0);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn title_key_when_no_url() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("d.db");
        let id = digest_id_days_ago(1, 1);
        insert_digest(&db, &id, &[sample_item("  Same   Title  ", None)]).unwrap();
        let seen = load_prior_day_seen_keys(&db).unwrap();
        let mut items = vec![freshness_cand("same title", None)];
        assert_eq!(filter_prior_day_seen(&mut items, &seen), 1);
        assert!(items.is_empty());
    }

    #[test]
    fn url_normalize_trailing_slash_and_host_case() {
        assert_eq!(
            normalize_digest_url("https://Example.COM/path/"),
            normalize_digest_url("https://example.com/path")
        );
        assert_eq!(
            digest_item_seen_key(Some("https://Example.COM/a/"), "x"),
            digest_item_seen_key(Some("https://example.com/a"), "y")
        );
    }

    #[test]
    fn press_hub_walk_skips_seen_tops() {
        let seen: HashSet<String> = [
            digest_item_seen_key(Some("https://news.example/a"), "A"),
            digest_item_seen_key(Some("https://news.example/b"), "B"),
        ]
        .into_iter()
        .collect();
        let links = vec![
            ("https://news.example/a".into(), "A".into()),
            ("https://news.example/b".into(), "B".into()),
            ("https://news.example/c".into(), "C".into()),
            ("https://news.example/d".into(), "D".into()),
        ];
        let kept = select_unseen_hub_links(links, &seen, 2);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].0, "https://news.example/c");
        assert_eq!(kept[1].0, "https://news.example/d");
    }

    #[test]
    fn rejects_listing_hub_paths_keeps_article_slug() {
        assert!(!looks_like_article_path(
            "https://www.cryptobreaking.com/category/news/markets-finance/"
        ));
        assert!(!looks_like_article_path(
            "https://openai.com/news/company-announcements/"
        ));
        assert!(!looks_like_article_path(
            "https://decrypt.co/news/cryptocurrencies"
        ));
        assert!(!looks_like_article_path(
            "https://decrypt.co/news/artificial-intelligence"
        ));
        assert!(!looks_like_article_path(
            "https://decrypt.co/news/machine-learning"
        ));
        assert!(looks_like_article_path(
            "https://cointelegraph.com/news/ecb-survey-0-2-euro-area-companies-accept-crypto-online"
        ));
        assert!(looks_like_article_path(
            "https://www.infoworld.com/article/a-brief-guide-to-ai-powered-software"
        ));
        assert!(looks_like_article_path(
            "https://decrypt.co/294851/bitcoin-etf-flows-hit-fresh-weekly-high-amid-rate-bets"
        ));
        assert!(looks_like_article_path(
            "https://www.cryptobreaking.com/morgan-stanley-data-shows-blackrock/"
        ));
        assert!(looks_like_article_path(
            "https://cointelegraph.com/markets/bitcoin-bottom-october-altcoins-basically-dead-swan-ceo"
        ));
        assert!(!looks_like_article_path(
            "https://cryptoactu.com/avis/gemini/"
        ));
    }

    #[test]
    fn listing_seo_copy_detected() {
        assert!(is_listing_seo_copy(
            "Markets & Finance",
            "Discover the latest stories, updates, and insights on Markets & Finance from Crypto Breaking News."
        ));
        assert!(is_listing_seo_copy(
            "Artificial Intelligence - Decrypt",
            "The latest AI news, features, and analysis, from generative to transformational technology."
        ));
        assert!(!is_listing_seo_copy(
            "ECB survey finds crypto rare",
            "An ECB survey of 8,205 euro area companies found just 0.2% of online sellers accept crypto."
        ));
    }

    #[test]
    fn twitter_search_author_cap_keeps_two() {
        let body = "I know a Rust shop that ships crates daily with care";
        let mut items = vec![
            Candidate {
                title: "Boardy @boardyai · a".into(),
                url: Some("https://x.com/boardyai/status/1".into()),
                subject: "a".into(),
                lane: "twitter".into(),
                weight: 9,
                detail: format!("{body} one"),
                query: "#rust".into(),
            },
            Candidate {
                title: "Boardy @boardyai · b".into(),
                url: Some("https://x.com/boardyai/status/2".into()),
                subject: "b".into(),
                lane: "twitter".into(),
                weight: 9,
                detail: format!("{body} two"),
                query: "#rust".into(),
            },
            Candidate {
                title: "Boardy @boardyai · c".into(),
                url: Some("https://x.com/boardyai/status/3".into()),
                subject: "c".into(),
                lane: "twitter".into(),
                weight: 9,
                detail: format!("{body} three"),
                query: "#rust".into(),
            },
            Candidate {
                title: "InfoWorld article long enough title".into(),
                url: Some("https://www.infoworld.com/article/a-long-enough-slug-here".into()),
                subject: "c".into(),
                lane: "live_site".into(),
                weight: 10,
                detail: String::new(),
                query: String::new(),
            },
        ];
        dedupe_twitter_search_authors(&mut items);
        assert_eq!(items.iter().filter(|c| c.lane == "twitter").count(), 2);
        assert_eq!(items.iter().filter(|c| c.lane == "live_site").count(), 1);
    }

    #[test]
    fn twitter_near_body_dedupes_clone_spam() {
        let body = "Python scientific core + Snakemake workflow + uv lockfile + reproducible container + pytest Hypothesis + Ruff mypy Pydantic + HDF5 outputs";
        let mut items = vec![
            Candidate {
                title: "August Wittorp @brick4956 · a".into(),
                url: Some("https://x.com/brick4956/status/1".into()),
                subject: "a".into(),
                lane: "twitter".into(),
                weight: 9,
                detail: format!("@GoogleDeepMind Is this a good idea {body}"),
                query: "#rust".into(),
            },
            Candidate {
                title: "August Wittorp @brick4956 · b".into(),
                url: Some("https://x.com/brick4956/status/2".into()),
                subject: "b".into(),
                lane: "twitter".into(),
                weight: 9,
                detail: format!("@SpaceXAI Is this a good idea {body}"),
                query: "#rust".into(),
            },
        ];
        dedupe_twitter_near_body(&mut items);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn press_listing_drop_keeps_twenty_from_pool() {
        let mut items: Vec<DigestItem> = (0..25)
            .map(|i| DigestItem {
                idx: i + 1,
                title: format!("solid article title number {i:02} long enough"),
                url: Some(format!(
                    "https://www.infoworld.com/article/solid-article-slug-number-{i:02}-here"
                )),
                subject: "s".into(),
                lane: "live_site".into(),
                weight: 8,
                detail:
                    "An ECB survey of companies found crypto rare at checkout with real analysis."
                        .into(),
            })
            .collect();
        items[0].detail =
            "Discover the latest stories, updates, and insights on Markets & Finance.".into();
        items[1].detail = "Stay up to speed on the rapid advancement of AI technology.".into();
        drop_listing_press_items(&mut items);
        items.truncate(MAX_SITE_ITEMS);
        assert_eq!(items.len(), MAX_SITE_ITEMS);
        assert!(!items
            .iter()
            .any(|it| is_listing_seo_copy(&it.title, &it.detail)));
    }

    #[test]
    fn following_lane_keeps_multiple_same_author() {
        let mut items = vec![
            Candidate {
                title: "Ada @ada · one".into(),
                url: Some("https://x.com/ada/status/1".into()),
                subject: "a".into(),
                lane: "following".into(),
                weight: 8,
                detail: "Shipped a small CLI that prints HTTP dates without pulling chrono today."
                    .into(),
                query: String::new(),
            },
            Candidate {
                title: "Ada @ada · two".into(),
                url: Some("https://x.com/ada/status/2".into()),
                subject: "b".into(),
                lane: "following".into(),
                weight: 8,
                detail: "Another distinct follow post about ratatui panes and status bars.".into(),
                query: String::new(),
            },
        ];
        dedupe_twitter_search_authors(&mut items);
        assert_eq!(items.len(), 2);
    }

    fn cand(title: &str, url: &str, lane: &str, weight: i32) -> Candidate {
        Candidate {
            title: title.into(),
            url: Some(url.into()),
            subject: title.into(),
            lane: lane.into(),
            weight,
            detail: String::new(),
            query: String::new(),
        }
    }

    #[test]
    fn mix_caps_press_and_tweets_with_hub_fairness() {
        let mut cands = Vec::new();
        for i in 0..15 {
            cands.push(cand(
                &format!("tweet {i} @user{i} long enough"),
                &format!("https://x.com/u{i}/status/{i}"),
                "twitter",
                9,
            ));
            cands.push(cand(
                &format!("infoworld article title number {i} here"),
                &format!("https://www.infoworld.com/article/{i}"),
                "live_site",
                8,
            ));
            cands.push(cand(
                &format!("zdnet article title number {i} here xx"),
                &format!("https://www.zdnet.com/article/{i}"),
                "live_site",
                3,
            ));
        }
        cands.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.title.cmp(&b.title)));
        let mixed = take_lane_mix(cands);
        let press: Vec<_> = mixed.iter().filter(|c| !is_x_lane(&c.lane)).collect();
        assert_eq!(press.len(), MAX_SITE_ITEMS);
        assert_eq!(mixed.iter().filter(|c| c.lane == "twitter").count(), 15);
        let iw = press
            .iter()
            .filter(|c| c.url.as_deref().is_some_and(|u| u.contains("infoworld")))
            .count();
        let zd = press
            .iter()
            .filter(|c| c.url.as_deref().is_some_and(|u| u.contains("zdnet")))
            .count();
        assert!(
            iw.abs_diff(zd) <= 2,
            "hubs should stay roughly balanced iw={iw} zd={zd}"
        );
        assert!(mixed[..press.len()].iter().all(|c| !is_x_lane(&c.lane)));
    }

    #[test]
    fn mix_keeps_home_and_search_caps_separate() {
        let mut cands = Vec::new();
        for i in 0..25 {
            cands.push(cand(
                &format!("foryou {i} @yuser{i} long enough"),
                &format!("https://x.com/y{i}/status/{i}"),
                "for_you",
                8,
            ));
            cands.push(cand(
                &format!("follow {i} @fuser{i} long enough"),
                &format!("https://x.com/f{i}/status/{i}"),
                "following",
                8,
            ));
            cands.push(cand(
                &format!("search {i} @suser{i} long enough"),
                &format!("https://x.com/s{i}/status/{i}"),
                "twitter",
                9,
            ));
        }
        let mixed = take_lane_mix(cands);
        assert_eq!(
            mixed.iter().filter(|c| c.lane == "for_you").count(),
            MAX_FOR_YOU_ITEMS
        );
        assert_eq!(
            mixed.iter().filter(|c| c.lane == "following").count(),
            MAX_FOLLOWING_ITEMS
        );
        assert_eq!(
            mixed.iter().filter(|c| c.lane == "twitter").count(),
            MAX_TWITTER_ITEMS
        );
        assert!(mixed.iter().all(|c| is_x_lane(&c.lane)));
    }

    #[test]
    fn digest_slack_post_splits_press_and_twitter_threads() {
        let rec = DigestRecord {
            digest_id: "DIGEST-20990101-000001".into(),
            status: "open".into(),
            created_at: String::new(),
            items: vec![
                DigestItem {
                    idx: 1,
                    title: "a brief guide to ai powered software.html".into(),
                    url: Some("https://www.infoworld.com/article/x".into()),
                    subject: "hub".into(),
                    lane: "live_site".into(),
                    weight: 8,
                    detail: "Signals keep form state in one reactive graph so templates stay declarative without event pipelines.".into(),
                },
                DigestItem {
                    idx: 2,
                    title: "Bikall Gurung @bikallem · 11h".into(),
                    url: Some("https://x.com/bikallem/status/1".into()),
                    subject: "rust".into(),
                    lane: "twitter".into(),
                    weight: 9,
                    detail: "Released http-date 0.1.0 - zero-dep Rust HTTP date parsing. Full crate notes and examples in the thread.".into(),
                },
                DigestItem {
                    idx: 3,
                    title: "Ada @ada · 2h".into(),
                    url: Some("https://x.com/ada/status/2".into()),
                    subject: "home".into(),
                    lane: "following".into(),
                    weight: 8,
                    detail: "Shipped a small CLI that prints HTTP dates without pulling chrono.".into(),
                },
                DigestItem {
                    idx: 4,
                    title: "Bea @bea · 1h".into(),
                    url: Some("https://x.com/bea/status/3".into()),
                    subject: "fy".into(),
                    lane: "for_you".into(),
                    weight: 8,
                    detail: "Algo feed item about rust tooling and crates.".into(),
                },
            ],
        };
        let post = digest_slack_post(&rec);
        assert_eq!(post.overview, "```\nDIGEST-20990101-000001\n```");
        assert!(!post.overview.contains("open"));
        assert!(!post.overview.contains("bare 3"));
        assert_eq!(post.press_title, "```\nPRESS 1\n```");
        assert_eq!(post.for_you_title, "```\nFOLLOWS FOR YOU 1\n```");
        assert_eq!(post.following_title, "```\nFOLLOWING 1\n```");
        assert_eq!(post.twitter_title, "```\nTWITTER 1\n```");
        assert_eq!(post.itc_title, "```\nINTERCHOUETTE 0\n```");
        assert!(post.itc_items.is_empty());
        assert_eq!(post.press_items.len(), 1);
        assert_eq!(post.for_you_items.len(), 1);
        assert_eq!(post.following_items.len(), 1);
        assert_eq!(post.twitter_items.len(), 1);
        assert!(post.press_items[0].contains("`1` _ *A Brief Guide To Ai Powered Software*"));
        assert!(post.press_items[0].contains("Signals keep form state"));
        assert!(post.press_items[0].contains("https://www.infoworld.com/article/x"));
        assert!(post.press_items[0].contains(
            "/propose_draft DIGEST-20990101-000001, 1\n/propose_tweet DIGEST-20990101-000001, 1"
        ));
        assert!(
            post.press_items[0].contains("`1` _ *A Brief Guide To Ai Powered Software*\n\n```\n")
        );
        assert!(!post.press_items[0].contains("aria-label"));
        assert!(!post.press_items[0].contains("```\n \n```"));
        assert!(!post.press_items[0].starts_with('\n'));
        assert!(post.twitter_items[0].contains("`2` _ *Bikall Gurung @bikallem*"));
        assert!(post.twitter_items[0].contains(
            "Released http-date 0.1.0 - zero-dep Rust HTTP date parsing. Full crate notes"
        ));
        assert!(!post.twitter_items[0].contains("View more"));
        assert!(!post.twitter_items[0].contains("```\n \n```"));
        assert!(post.following_items[0].contains("`3` _ *Ada @ada*"));
        assert!(post.following_items[0].contains("Shipped a small CLI"));
        assert!(post.for_you_items[0].contains("`4` _ *Bea @bea*"));
    }

    #[test]
    fn digest_slack_post_includes_interchouette_lane() {
        let rec = DigestRecord {
            digest_id: "DIGEST-20990101-000010".into(),
            status: "open".into(),
            created_at: String::new(),
            items: vec![
                DigestItem {
                    idx: 1,
                    title: "DRAFT · itcy-tui".into(),
                    url: Some("https://github.com/Interchouette-ITC/itcy-tui".into()),
                    subject: "itcy-tui".into(),
                    lane: "itc_draft".into(),
                    weight: 9,
                    detail: "Ratatui status pane for the always-on binary.".into(),
                },
                DigestItem {
                    idx: 2,
                    title: "TWEET · tvscreener-rs".into(),
                    url: Some("https://github.com/Interchouette-ITC/tvscreener-rs".into()),
                    subject: "tvscreener-rs".into(),
                    lane: "itc_tweet".into(),
                    weight: 8,
                    detail: "Rust screener crates for market data.".into(),
                },
            ],
        };
        let post = digest_slack_post(&rec);
        assert_eq!(post.itc_title, "```\nINTERCHOUETTE 2\n```");
        assert_eq!(post.itc_items.len(), 2);
        assert!(post.itc_items[0].contains("DRAFT · itcy-tui"));
        assert!(post.itc_items[0].contains("https://github.com/Interchouette-ITC/itcy-tui"));
        assert!(post.itc_items[0].contains(
            "/propose_draft DIGEST-20990101-000010, 1\n/propose_tweet DIGEST-20990101-000010, 1"
        ));
        assert!(post.itc_items[1].contains("TWEET · tvscreener-rs"));
        assert!(post.itc_items[1].contains(
            "/propose_draft DIGEST-20990101-000010, 2\n/propose_tweet DIGEST-20990101-000010, 2"
        ));
    }

    #[test]
    fn grey_bar_sits_between_items_not_inside() {
        let rec = DigestRecord {
            digest_id: "DIGEST-20990101-000002".into(),
            status: "open".into(),
            created_at: String::new(),
            items: vec![
                DigestItem {
                    idx: 1,
                    title: "first article title here".into(),
                    url: Some("https://www.infoworld.com/article/a".into()),
                    subject: "a".into(),
                    lane: "live_site".into(),
                    weight: 8,
                    detail: "Signals keep form state in one reactive graph so templates stay declarative without event pipelines.".into(),
                },
                DigestItem {
                    idx: 2,
                    title: "second article title here".into(),
                    url: Some("https://www.zdnet.com/article/b".into()),
                    subject: "b".into(),
                    lane: "live_site".into(),
                    weight: 7,
                    detail: "Kernel updates land in the stable tree with a short note on regression risk for distro backports.".into(),
                },
            ],
        };
        let post = digest_slack_post(&rec);
        assert_eq!(post.press_items.len(), 2);
        assert!(!post.press_items[0].contains(DIGEST_ITEM_GREY_BAR));
        let msgs = digest_slack_messages(&rec);
        assert_eq!(
            msgs.iter().filter(|m| *m == DIGEST_ITEM_GREY_BAR).count(),
            1
        );
        let first = msgs.iter().position(|m| m.contains("article/a")).unwrap();
        let bar = msgs.iter().position(|m| m == DIGEST_ITEM_GREY_BAR).unwrap();
        let second = msgs.iter().position(|m| m.contains("article/b")).unwrap();
        assert!(first < bar && bar < second);
    }

    #[test]
    fn digest_item_url_is_propose_draft_forced_cite() {
        let it = DigestItem {
            idx: 12,
            title: "A Third of the Post-ChatGPT Web Is AI-Written, Pew Finds".into(),
            url: Some("https://decrypt.co/376271/chatgpt-web-ai-written-pew".into()),
            subject: "third post-chatgpt web ai-written pew finds".into(),
            lane: "for_you".into(),
            weight: 1,
            detail: "Pew Research scanned nearly half a million webpages with an AI detector and found the fingerprints of AI everywhere.".into(),
        };
        let cite = it.url.as_deref().filter(|u| !u.trim().is_empty());
        assert_eq!(
            cite,
            Some("https://decrypt.co/376271/chatgpt-web-ai-written-pew")
        );
        let (_topic, instructions) = digest_propose_brief(&it);
        assert!(instructions.contains("decrypt.co/376271"));
    }

    #[test]
    fn digest_propose_brief_uses_detail_and_url() {
        let it = DigestItem {
            idx: 40,
            title: "ayush @ayushagarwal027 · Came across a Rust LSP".into(),
            url: Some("https://x.com/ayushagarwal027/status/2090736100025504071".into()),
            subject: "Came across a Rust LSP that stays under".into(),
            lane: "for_you".into(),
            weight: 1,
            detail: "Came across a Rust LSP that stays under 100MB of RAM, and instantly resumes indexing after restart.\n\nRust Glancer is a 4-month-old alternative to rust-analyzer.\n\nThe motivation was".into(),
        };
        let (topic, instructions) = digest_propose_brief(&it);
        assert!(
            topic.contains("100MB"),
            "topic from full detail, not truncated subject: {topic}"
        );
        assert!(
            instructions.contains("Rust Glancer"),
            "instructions keep card body: {instructions}"
        );
        assert!(
            instructions.contains("2090736100025504071"),
            "instructions keep item URL: {instructions}"
        );
    }

    #[test]
    fn take_lane_mix_drops_linkedin_urls() {
        let mixed = take_lane_mix(vec![
            cand(
                "Li comment long enough title",
                "https://www.linkedin.com/posts/x",
                "live_site",
                9,
            ),
            cand(
                "InfoWorld article long enough title",
                "https://www.infoworld.com/article/x",
                "live_site",
                8,
            ),
        ]);
        assert_eq!(mixed.len(), 1);
        assert!(mixed[0]
            .url
            .as_deref()
            .is_some_and(|u| u.contains("infoworld")));
    }

    #[test]
    fn press_skips_grey_when_detail_is_hub_chrome() {
        let it = DigestItem {
            idx: 1,
            title: "a real article title here".into(),
            url: Some("https://www.infoworld.com/article/x".into()),
            subject: "hub".into(),
            lane: "live_site".into(),
            weight: 8,
            detail: "infoworld.com\ndev / languages / enterprise software".into(),
        };
        let msg = format_digest_item(&it, "DIGEST-20990101-000099");
        assert!(!msg.contains("```"));
        assert!(msg.contains("https://www.infoworld.com/article/x"));
        assert!(msg.contains(
            "/propose_draft DIGEST-20990101-000099, 1\n/propose_tweet DIGEST-20990101-000099, 1"
        ));
    }
}

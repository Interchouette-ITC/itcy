// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! List / show / delete saved DRAFT- and TWEET- rows; re-post DIGEST- to `#daily-digest`.
//!
//! After BAT ship, rows stay as `published`. `/list` shows them under Posts / X posts
//! (display ids `POST-…` / `XPOST-…`). `/show` accepts DRAFT/TWEET/POST/XPOST.

use crate::bat::github::{github_owner_from_pr_url, BatGithubConfig, ClosePrOutcome, GithubClient};
use crate::bat::store::{status, DraftStore, StoredDraft};
use crate::slack::api::post_digest_channel;
use crate::slack::handler::SlackRuntime;
use crate::sources::digest::{digest_slack_post, format_digest_slack, get_digest};
use std::fmt::Write;
use tracing::{info, warn};

impl SlackRuntime {
    pub(crate) fn list_saved_reply(&self) -> String {
        let drafts = self.list_saved_section("DRAFT-", false, "Drafts");
        let posts = self.list_saved_section("DRAFT-", true, "Posts");
        let tweets = self.list_saved_section("TWEET-", false, "Tweets");
        let xposts = self.list_saved_section("TWEET-", true, "X posts");
        let creplies = self.list_saved_section("CREPLY-", false, "LinkedIn replies");
        let xreplies = self.list_saved_section("XREPLY-", false, "X replies");
        let shipped_c = self.list_saved_section("CREPLY-", true, "Shipped LinkedIn replies");
        let shipped_x = self.list_saved_section("XREPLY-", true, "Shipped X replies");
        format!(
            "{drafts}\n\n{posts}\n\n{tweets}\n\n{xposts}\n\n{creplies}\n\n{xreplies}\n\n{shipped_c}\n\n{shipped_x}"
        )
    }

    pub(crate) async fn show_saved_ids_reply(&self, ids: &[String]) -> String {
        let mut parts = Vec::with_capacity(ids.len());
        for id in ids {
            parts.push(self.show_one_saved(id).await);
        }
        parts.join("\n\n---\n\n")
    }

    pub(crate) async fn delete_saved_ids_reply(&self, ids: &[String]) -> String {
        let mut parts = Vec::with_capacity(ids.len());
        for id in ids {
            parts.push(self.delete_one_saved(id).await);
        }
        parts.join("\n")
    }

    fn list_saved_section(&self, prefix: &str, published_only: bool, title: &str) -> String {
        let store = match DraftStore::open(&self.config.state_db_path) {
            Ok(s) => s,
            Err(e) => return format!("Could not open store: {e}"),
        };
        match store.list_by_id_prefix(prefix, 50) {
            Ok(rows) => {
                let filtered: Vec<StoredDraft> = rows
                    .into_iter()
                    .filter(|r| {
                        let is_pub = r.status == status::PUBLISHED;
                        if published_only {
                            is_pub
                        } else {
                            !is_pub
                        }
                    })
                    .take(30)
                    .collect();
                if filtered.is_empty() {
                    format!("No saved {title}.")
                } else {
                    format_saved_list(title, &filtered, published_only)
                }
            }
            Err(e) => format!("Could not list {title}: {e}"),
        }
    }

    async fn show_one_saved(&self, id: &str) -> String {
        if id.starts_with("DIGEST-") {
            return self.show_digest_reply(id).await;
        }
        let store_id = resolve_store_id(id);
        let Some(kind) = kind_for_id(&store_id) else {
            return format!(
                "unknown id `{id}` (need DRAFT-…, POST-…, TWEET-…, XPOST-…, CREPLY-…, XREPLY-…, or DIGEST-…)"
            );
        };
        let store = match DraftStore::open(&self.config.state_db_path) {
            Ok(s) => s,
            Err(e) => return format!("Could not open store: {e}"),
        };
        match store.get(&store_id) {
            Ok(Some(row)) => {
                let display = operator_display_id(&row);
                let restored = crate::llm::disclosure::ensure_stored_disclosure(
                    row.body.trim(),
                    &row.model,
                    row.tokens_in,
                    row.tokens_out,
                );
                if row.draft_id.starts_with("DRAFT-")
                    && (row.status == status::ACCEPTED || row.status == status::PUBLISHED)
                {
                    let paste = crate::sources::draft_footer::linkedin_manual_paste_message(
                        &row.body,
                        &row.model,
                        row.tokens_in,
                        row.tokens_out,
                    );
                    let next = reply_or_draft_next(&display, &row);
                    let pr_footer = pending_pr_footer_for_row(&row).await;
                    format!(
                        "{kind} `{display}`  {semoji} status=`{st}`  updated=`{upd}`\n\n{paste}\n\n{next}{pr_footer}",
                        kind = title_case_for_row(&row),
                        semoji = status_emoji(&row.status),
                        st = row.status,
                        upd = row.updated_at,
                    )
                } else {
                    let body = if restored.trim().is_empty() {
                        "(empty body)".to_string()
                    } else if row.draft_id.starts_with("DRAFT-") {
                        crate::sources::draft_footer::slack_paste_safe_linkedin_message(&restored)
                    } else if crate::sources::reply_comment::is_reply_id(&row.draft_id) {
                        let prose = if row.draft_id.starts_with("XREPLY-") {
                            crate::publish::tweet_text_for_api(&restored)
                        } else {
                            crate::publish::linkedin_text_for_api(&restored)
                        };
                        crate::sources::draft_footer::slack_paste_safe_reply_body(&prose)
                    } else {
                        crate::sources::draft_footer::slack_highlight_active_link(&restored)
                    };
                    let next = reply_or_draft_next(&display, &row);
                    let pr_footer = pending_pr_footer_for_row(&row).await;
                    if next.is_empty() {
                        format!(
                            "{kind} `{display}`  {semoji} status=`{st}`  updated=`{upd}`\n\n{body}{pr_footer}",
                            kind = title_case_for_row(&row),
                            semoji = status_emoji(&row.status),
                            st = row.status,
                            upd = row.updated_at,
                        )
                    } else {
                        format!(
                            "{kind} `{display}`  {semoji} status=`{st}`  updated=`{upd}`\n\n{body}\n\n{next}{pr_footer}",
                            kind = title_case_for_row(&row),
                            semoji = status_emoji(&row.status),
                            st = row.status,
                            upd = row.updated_at,
                        )
                    }
                }
            }
            Ok(None) => format!("No {kind} `{id}`."),
            Err(e) => format!("Could not load {kind}: {e}"),
        }
    }

    async fn show_digest_reply(&self, digest_id: &str) -> String {
        let db = self.config.state_db_path.as_path();
        let rec = match get_digest(db, digest_id) {
            Ok(Some(r)) => r,
            Ok(None) => return format!("No digest `{digest_id}` in runtime.db."),
            Err(e) => return format!("Could not load digest: {e}"),
        };
        let digest_ch = self.config.daily_digest_channel_id.trim();
        if digest_ch.is_empty() {
            return format!(
                "{text}\n\n_(Set SLACK_DAILY_DIGEST_CHANNEL_ID to post into #daily-digest; replied here only.)_",
                text = format_digest_slack(&rec)
            );
        }
        let post = digest_slack_post(&rec);
        match post_digest_channel(&self.config.bot_token, digest_ch, &post).await {
            Ok(()) => format!("Re-posted `{id}` to #daily-digest.", id = rec.digest_id),
            Err(e) => format!(
                "Digest `{id}` loaded but Slack post to #daily-digest failed: {e}",
                id = rec.digest_id
            ),
        }
    }

    async fn delete_one_saved(&self, id: &str) -> String {
        let store_id = resolve_store_id(id);
        let Some(kind) = kind_for_id(&store_id) else {
            return format!(
                "unknown id `{id}` (need DRAFT-…, POST-…, TWEET-…, XPOST-…, CREPLY-…, or XREPLY-…)"
            );
        };
        let store = match DraftStore::open(&self.config.state_db_path) {
            Ok(s) => s,
            Err(e) => return format!("Could not open store: {e}"),
        };
        let row = match store.get(&store_id) {
            Ok(Some(r)) => r,
            Ok(None) => return format!("No {kind} `{id}`."),
            Err(e) => return format!("Could not load {kind}: {e}"),
        };
        let display = operator_display_id(&row);
        let pr_note = if row.status == status::PUBLISHED {
            String::new()
        } else {
            close_row_pr(&row).await
        };
        info!(
            id = %row.draft_id,
            status = %row.status,
            pr = row.fork_pr_number.unwrap_or(0),
            "saved: deleting row"
        );
        match store.delete(&store_id) {
            Ok(true) => {
                let mut out = format!("Deleted {kind} `{display}` (was `{st}`).", st = row.status);
                if !pr_note.is_empty() {
                    out.push('\n');
                    out.push_str(&pr_note);
                }
                out
            }
            Ok(false) => format!("No {kind} `{id}`."),
            Err(e) => format!("Could not delete {kind}: {e}"),
        }
    }
}

/// Map operator `POST-` / `XPOST-` ids to the stored `DRAFT-` / `TWEET-` primary key.
#[must_use]
pub(crate) fn resolve_store_id(id: &str) -> String {
    match id.split_once('-') {
        Some(("POST", rest)) => format!("DRAFT-{rest}"),
        Some(("XPOST", rest)) => format!("TWEET-{rest}"),
        _ => id.to_string(),
    }
}

/// Operator-facing id: published drafts/tweets show as `POST-` / `XPOST-`.
#[must_use]
pub(crate) fn operator_display_id(row: &StoredDraft) -> String {
    if row.status != status::PUBLISHED {
        return row.draft_id.clone();
    }
    match row.draft_id.split_once('-') {
        Some(("DRAFT", rest)) => format!("POST-{rest}"),
        Some(("TWEET", rest)) => format!("XPOST-{rest}"),
        _ => row.draft_id.clone(),
    }
}

fn kind_for_id(id: &str) -> Option<&'static str> {
    if id.starts_with("TWEET-") || id.starts_with("XPOST-") {
        Some("tweet")
    } else if id.starts_with("DRAFT-") || id.starts_with("POST-") {
        Some("draft")
    } else if id.starts_with("CREPLY-") {
        Some("linkedin reply")
    } else if id.starts_with("XREPLY-") {
        Some("x reply")
    } else {
        None
    }
}

fn title_case_for_row(row: &StoredDraft) -> &'static str {
    if row.draft_id.starts_with("CREPLY-") {
        if row.status == status::PUBLISHED {
            "Shipped LinkedIn reply"
        } else {
            "LinkedIn reply"
        }
    } else if row.draft_id.starts_with("XREPLY-") {
        if row.status == status::PUBLISHED {
            "Shipped X reply"
        } else {
            "X reply"
        }
    } else if row.status == status::PUBLISHED {
        if row.draft_id.starts_with("TWEET-") {
            "X post"
        } else {
            "Post"
        }
    } else if row.draft_id.starts_with("TWEET-") {
        "Tweet"
    } else {
        "Draft"
    }
}

/// Operator `Next:` block (same shape as after `/draft_about` / `/tweet_about`), by row status.
#[must_use]
pub(crate) fn next_slash_hints(id: &str, row_status: &str) -> String {
    match row_status {
        status::OPEN | status::BUILDING => format!(
            ":point_right: Next:\n\n\
:pencil2: /rework {id} <instructions>\n\n\
:link: /change_url {id} 1\n\n\
:white_check_mark: /accept {id}"
        ),
        status::ACCEPTED => format!(
            ":point_right: Next:\n\n\
:pencil2: /rework {id} <instructions>\n\n\
:link: /change_url {id} 1\n\n\
:white_check_mark: /accept {id}\n\n\
:repeat: /retry_bat {id}"
        ),
        status::PUBLISHED => format!(
            ":point_right: Next:\n\n\
:repeat: /retry_bat {id}\n\n\
:mag: /show {id}\n\n\
:wastebasket: /delete {id}"
        ),
        status::FAILED => format!(
            ":point_right: Next:\n\n\
:mag: /show {id}\n\n\
:wastebasket: /delete {id}"
        ),
        _ => String::new(),
    }
}

fn reply_or_draft_next(display: &str, row: &StoredDraft) -> String {
    if crate::sources::reply_comment::is_reply_id(&row.draft_id) {
        crate::sources::reply_comment::reply_next_hints(display, &row.status)
    } else {
        next_slash_hints(display, &row.status)
    }
}

fn status_emoji(st: &str) -> &'static str {
    match st {
        status::OPEN => ":large_green_circle:",
        status::BUILDING => ":gear:",
        status::ACCEPTED => ":hourglass_flowing_sand:",
        status::PUBLISHED => ":white_check_mark:",
        status::FAILED => ":x:",
        _ => ":grey_question:",
    }
}

fn format_saved_list(title: &str, rows: &[StoredDraft], as_posts: bool) -> String {
    let mut out = format!(":clipboard: {title} ({n}, newest first):\n", n = rows.len());
    for row in rows {
        let subj = clip_list_subject(&row.subject);
        let id = if as_posts {
            operator_display_id(row)
        } else {
            row.draft_id.clone()
        };
        let _ = writeln!(
            out,
            "`{id}` {semoji} `{st}` - {subj}",
            semoji = status_emoji(&row.status),
            st = row.status,
        );
    }
    let _ = write!(
        out,
        ":mag: Show: `/show <ID>, <ID>`\n:wastebasket: Delete: `/delete <ID>, <ID>`"
    );
    out
}

fn clip_list_subject(subject: &str) -> String {
    const MAX: usize = 72;
    let one_line = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= MAX {
        return one_line;
    }
    let mut out = String::new();
    for ch in one_line.chars() {
        if out.chars().count() + 1 >= MAX.saturating_sub(1) {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

struct PendingPrLink {
    label: &'static str,
    url: String,
}

/// Footer `:link:` lines for open PRs still waiting gRoussac Approve (`/show` after Next block).
async fn pending_pr_footer_for_row(row: &StoredDraft) -> String {
    let links = pending_pr_links_for_row(row).await;
    format_pending_pr_footer(&links)
}

async fn pending_pr_links_for_row(row: &StoredDraft) -> Vec<PendingPrLink> {
    if row.status != status::ACCEPTED {
        return Vec::new();
    }
    let Ok(cfg) = BatGithubConfig::from_env() else {
        return fallback_pending_links(row);
    };
    let Ok(client) = GithubClient::new(cfg.clone()) else {
        return fallback_pending_links(row);
    };
    let mut links = Vec::new();
    if let (Some(n), url) = (row.fork_pr_number, row.fork_pr_url.as_str()) {
        if !url.is_empty() {
            let owner = github_owner_from_pr_url(url).unwrap_or(&cfg.drafts_owner);
            let approved = client
                .bat_readiness_on(owner, n)
                .await
                .is_ok_and(|r| r.approved);
            if !approved {
                links.push(PendingPrLink {
                    label: "Fork BAT",
                    url: url.to_string(),
                });
            }
        }
    }
    links
}

/// When GitHub is unavailable, still remind from stored fork URL while status is `accepted`.
fn fallback_pending_links(row: &StoredDraft) -> Vec<PendingPrLink> {
    if row.status != status::ACCEPTED {
        return Vec::new();
    }
    if row.fork_pr_url.is_empty() {
        return Vec::new();
    }
    vec![PendingPrLink {
        label: "Fork BAT",
        url: row.fork_pr_url.clone(),
    }]
}

fn format_pending_pr_footer(links: &[PendingPrLink]) -> String {
    if links.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for link in links {
        let _ = write!(out, "\n:link: {}: {}", link.label, link.url);
    }
    out
}

async fn close_row_pr(row: &StoredDraft) -> String {
    let Some(pr) = row.fork_pr_number.filter(|n| *n > 0) else {
        return String::new();
    };
    let Ok(cfg) = BatGithubConfig::from_env() else {
        return format!("(could not close fork PR #{pr}: missing GitHub config)");
    };
    let Ok(client) = GithubClient::new(cfg) else {
        return format!("(could not close fork PR #{pr}: GitHub client)");
    };
    let outcome = if row.draft_id.starts_with("TWEET-") {
        client.close_tweet_pr(pr).await
    } else {
        client.close_draft_pr(pr).await
    };
    match outcome {
        Ok(ClosePrOutcome::Closed) => format!("Closed fork PR #{pr}."),
        Ok(ClosePrOutcome::AlreadyClosed) => format!("Fork PR #{pr} was already closed."),
        Err(e) => {
            warn!(error = %e, pr, "saved: close fork PR failed");
            format!("(could not close fork PR #{pr}: {e})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_post_xpost_to_store_ids() {
        assert_eq!(
            resolve_store_id("POST-20260822-000089"),
            "DRAFT-20260822-000089"
        );
        assert_eq!(
            resolve_store_id("XPOST-20260825-000073"),
            "TWEET-20260825-000073"
        );
        assert_eq!(
            resolve_store_id("DRAFT-20260822-000089"),
            "DRAFT-20260822-000089"
        );
    }

    #[test]
    fn published_display_ids() {
        let mut draft = StoredDraft {
            draft_id: "DRAFT-20260822-000089".into(),
            subject: "x".into(),
            body: "y".into(),
            model: String::new(),
            tokens_in: 0,
            tokens_out: 0,
            sources: vec![],
            link_options: vec![],
            research_pack: String::new(),
            status: status::PUBLISHED.into(),
            created_at: String::new(),
            updated_at: String::new(),
            fork_pr_number: None,
            fork_pr_url: String::new(),
        };
        assert_eq!(operator_display_id(&draft), "POST-20260822-000089");
        draft.draft_id = "TWEET-20260825-000073".into();
        assert_eq!(operator_display_id(&draft), "XPOST-20260825-000073");
        draft.status = status::OPEN.into();
        assert_eq!(operator_display_id(&draft), "TWEET-20260825-000073");
    }

    #[test]
    fn next_slash_hints_published() {
        let n = next_slash_hints("POST-20260822-000089", status::PUBLISHED);
        assert!(n.contains("/show POST-20260822-000089"));
        assert!(n.contains("/retry_bat POST-20260822-000089"));
        assert!(n.contains("/delete POST-20260822-000089"));
    }

    #[test]
    fn pending_pr_footer_formats_link_lines() {
        let links = [
            PendingPrLink {
                label: "Fork BAT",
                url: "https://github.com/Interchouette/itcy-publications/pull/58".into(),
            },
            PendingPrLink {
                label: "Org drafts",
                url: "https://github.com/Interchouette-ITC/itcy-publications/pull/101".into(),
            },
        ];
        let footer = format_pending_pr_footer(&links);
        assert!(footer.contains(":link: Fork BAT:"), "{footer}");
        assert!(footer.contains(":link: Org drafts:"), "{footer}");
        assert!(footer.contains("pull/58"), "{footer}");
        assert!(footer.contains("pull/101"), "{footer}");
        assert!(format_pending_pr_footer(&[]).is_empty());
    }
}

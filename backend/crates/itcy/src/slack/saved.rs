// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! List / show / delete saved DRAFT- and TWEET- rows; re-post DIGEST- to `#daily-digest`.

use crate::bat::github::{BatGithubConfig, ClosePrOutcome, GithubClient};
use crate::bat::store::{status, DraftStore, StoredDraft};
use crate::slack::api::post_digest_channel;
use crate::slack::handler::SlackRuntime;
use crate::sources::digest::{digest_slack_post, format_digest_slack, get_digest};
use std::fmt::Write;
use tracing::{info, warn};

impl SlackRuntime {
    pub(crate) fn list_saved_reply(&self) -> String {
        let drafts = self.list_saved_section("DRAFT-", "Drafts");
        let tweets = self.list_saved_section("TWEET-", "Tweets");
        format!("{drafts}\n\n{tweets}")
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

    fn list_saved_section(&self, prefix: &str, title: &str) -> String {
        let store = match DraftStore::open(&self.config.state_db_path) {
            Ok(s) => s,
            Err(e) => return format!("Could not open store: {e}"),
        };
        match store.delete_prefix_status(prefix, status::PUBLISHED) {
            Ok(0) => {}
            Ok(n) => info!(n, prefix, "saved: dropped published rows"),
            Err(e) => warn!(error = %e, prefix, "saved: drop published failed"),
        }
        match store.list_by_id_prefix(prefix, 30) {
            Ok(rows) if rows.is_empty() => format!("No saved {title}."),
            Ok(rows) => format_saved_list(title, &rows),
            Err(e) => format!("Could not list {title}: {e}"),
        }
    }

    async fn show_one_saved(&self, id: &str) -> String {
        if id.starts_with("DIGEST-") {
            return self.show_digest_reply(id).await;
        }
        let Some(kind) = kind_for_id(id) else {
            return format!("unknown id `{id}` (need DRAFT-…, TWEET-…, or DIGEST-…)");
        };
        let store = match DraftStore::open(&self.config.state_db_path) {
            Ok(s) => s,
            Err(e) => return format!("Could not open store: {e}"),
        };
        match store.get(id) {
            Ok(Some(row)) => {
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
                    let next = next_slash_hints(&row.draft_id, &row.status);
                    format!(
                        "{kind} `{id}`  {semoji} status=`{st}`  updated=`{upd}`\n\n{paste}\n\n{next}",
                        kind = title_case(kind),
                        id = row.draft_id,
                        semoji = status_emoji(&row.status),
                        st = row.status,
                        upd = row.updated_at,
                    )
                } else {
                    let body = if restored.trim().is_empty() {
                        "(empty body)".to_string()
                    } else if row.draft_id.starts_with("DRAFT-") {
                        crate::sources::draft_footer::slack_paste_safe_linkedin_message(&restored)
                    } else {
                        crate::sources::draft_footer::slack_highlight_active_link(&restored)
                    };
                    let next = next_slash_hints(&row.draft_id, &row.status);
                    if next.is_empty() {
                        format!(
                            "{kind} `{id}`  {semoji} status=`{st}`  updated=`{upd}`\n\n{body}",
                            kind = title_case(kind),
                            id = row.draft_id,
                            semoji = status_emoji(&row.status),
                            st = row.status,
                            upd = row.updated_at,
                        )
                    } else {
                        format!(
                            "{kind} `{id}`  {semoji} status=`{st}`  updated=`{upd}`\n\n{body}\n\n{next}",
                            kind = title_case(kind),
                            id = row.draft_id,
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
        let Some(kind) = kind_for_id(id) else {
            return format!("unknown id `{id}` (need DRAFT-… or TWEET-…)");
        };
        let store = match DraftStore::open(&self.config.state_db_path) {
            Ok(s) => s,
            Err(e) => return format!("Could not open store: {e}"),
        };
        let row = match store.get(id) {
            Ok(Some(r)) => r,
            Ok(None) => return format!("No {kind} `{id}`."),
            Err(e) => return format!("Could not load {kind}: {e}"),
        };
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
        match store.delete(id) {
            Ok(true) => {
                let mut out = format!(
                    "Deleted {kind} `{id}` (was `{st}`).",
                    id = row.draft_id,
                    st = row.status
                );
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

fn kind_for_id(id: &str) -> Option<&'static str> {
    if id.starts_with("TWEET-") {
        Some("tweet")
    } else if id.starts_with("DRAFT-") {
        Some("draft")
    } else {
        None
    }
}

fn title_case(kind: &str) -> &'static str {
    match kind {
        "tweet" => "Tweet",
        _ => "Draft",
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
:mag: /show {id}"
        ),
        status::FAILED => format!(
            ":point_right: Next:\n\n\
:mag: /show {id}\n\n\
:wastebasket: /delete {id}"
        ),
        _ => String::new(),
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

fn format_saved_list(title: &str, rows: &[StoredDraft]) -> String {
    let mut out = format!(":clipboard: {title} ({n}, newest first):\n", n = rows.len());
    for row in rows {
        let subj = clip_list_subject(&row.subject);
        let _ = writeln!(
            out,
            "`{id}` {semoji} `{st}` - {subj}",
            id = row.draft_id,
            semoji = status_emoji(&row.status),
            st = row.status,
        );
    }
    let _ = write!(
        out,
        "\n:mag: Show: /show <ID>, <ID>\n:wastebasket: Delete: /delete <ID>, <ID>"
    );
    out
}

fn clip_list_subject(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= 60 {
        one
    } else {
        format!("{}...", one.chars().take(57).collect::<String>())
    }
}

async fn close_row_pr(row: &StoredDraft) -> String {
    let Some(n) = row.fork_pr_number.filter(|n| *n > 0) else {
        return String::new();
    };
    let cfg = match BatGithubConfig::from_env() {
        Ok(c) => c,
        Err(e) => return format!("Could not close GitHub PR #{n}: {e}"),
    };
    let client = match GithubClient::new(cfg) {
        Ok(c) => c,
        Err(e) => return format!("Could not close GitHub PR #{n}: {e}"),
    };
    let res = if row.draft_id.starts_with("TWEET-") {
        client.close_tweet_pr(n).await
    } else {
        client.close_draft_pr(n).await
    };
    match res {
        Ok(ClosePrOutcome::Closed) => {
            info!(pr = n, id = %row.draft_id, "saved: closed GitHub PR");
            format!("Closed GitHub PR #{n}.")
        }
        Ok(ClosePrOutcome::AlreadyClosed) => format!("GitHub PR #{n} was already closed."),
        Err(e) => {
            warn!(error = %e, pr = n, id = %row.draft_id, "saved: close GitHub PR failed");
            format!("Could not close GitHub PR #{n}: {e}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::next_slash_hints;
    use crate::bat::store::status;

    #[test]
    fn next_hints_open_has_rework_change_accept() {
        let n = next_slash_hints("DRAFT-20260824-000093", status::OPEN);
        assert!(n.contains("/rework DRAFT-20260824-000093"));
        assert!(n.contains("/change_url DRAFT-20260824-000093 1"));
        assert!(n.contains("/accept DRAFT-20260824-000093"));
        assert!(!n.contains("/retry_bat"));
    }

    #[test]
    fn next_hints_accepted_adds_retry_bat() {
        let n = next_slash_hints("TWEET-20260824-000001", status::ACCEPTED);
        assert!(n.contains("/rework TWEET-20260824-000001"));
        assert!(n.contains("/retry_bat TWEET-20260824-000001"));
    }
}

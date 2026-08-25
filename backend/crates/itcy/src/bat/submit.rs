// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Submit / retry BAT for Draft PRs (`<DRAFT-id>/` on the drafts branch).

use crate::bat::github::{
    draft_pr_body, tweet_pr_body, BatGithubConfig, GithubClient, GithubError, OpenedPr, RepoFile,
};
use crate::bat::pack::{
    branch_name_for_draft, branch_name_for_tweet, pack_draft_files, pack_tweet_files,
};
use crate::bat::store::{status, DraftStore, DraftStoreError, StoredDraft};
use std::path::Path;
use thiserror::Error;

/// Outcome of a successful Draft submit on the Interchouette fork.
#[derive(Debug, Clone)]
pub struct BatSubmitResult {
    pub pr_url: String,
    pub pr_number: u64,
    pub branch: String,
    pub draft_id: String,
    /// True when files were updated on an existing PR (not newly opened).
    pub updated_existing: bool,
    /// Set when Approve was already on GitHub and the Post was published (webhook-miss recovery).
    pub promoted: Option<RetryBatResult>,
}

/// Outcome of publishing the Post after BAT (`/retry_bat` or auto after re-`/accept_draft`).
#[derive(Debug, Clone)]
pub struct RetryBatResult {
    pub draft_id: String,
    pub post_id: String,
    pub pr_number: u64,
    pub detail: String,
}

#[derive(Debug, Error)]
pub enum BatSubmitError {
    #[error("{0}")]
    Store(#[from] DraftStoreError),
    #[error("{0}")]
    Github(#[from] GithubError),
    #[error("no saved `{0}` and no Post/XPOST on publications to re-ship")]
    NoDraft(String),
    #[error("{0}")]
    Gate(String),
}

/// Opens or updates the fork Draft PR. Idempotent on **`open`** and **`accepted`**.
///
/// If the PR already exists (`accepted`, or `open` with a known fork PR), pushes pack files
/// again and keeps / restores `accepted`. When gRoussac Approve is already on GitHub
/// (webhook missed), publishes the org Post in the same call.
///
/// # Errors
///
/// Returns [`BatSubmitError`] when the draft cannot be accepted or GitHub fails.
pub async fn accept_draft(
    db_path: &Path,
    draft_id: &str,
) -> Result<BatSubmitResult, BatSubmitError> {
    if crate::sources::reply_comment::is_reply_id(draft_id) {
        return Err(BatSubmitError::Gate(
            "CREPLY-/XREPLY- ship via `/accept` (direct); not BAT".into(),
        ));
    }
    if draft_id.starts_with("TWEET-") {
        return Err(BatSubmitError::Gate(
            "use `/accept_tweet` for TWEET- ids (not `/accept_draft`)".into(),
        ));
    }
    accept_surface(db_path, draft_id, BatSurface::LinkedIn).await
}

/// Opens or updates the fork Tweet PR into **`drafts_tweet`**. Idempotent on **`open`** and **`accepted`**.
///
/// # Errors
///
/// Returns [`BatSubmitError`] when the tweet cannot be accepted or GitHub fails.
pub async fn accept_tweet(
    db_path: &Path,
    tweet_id: &str,
) -> Result<BatSubmitResult, BatSubmitError> {
    if crate::sources::reply_comment::is_reply_id(tweet_id) {
        return Err(BatSubmitError::Gate(
            "CREPLY-/XREPLY- ship via `/accept` (direct); not BAT".into(),
        ));
    }
    if tweet_id.starts_with("DRAFT-") {
        return Err(BatSubmitError::Gate(
            "use `/accept_draft` for DRAFT- ids (not `/accept_tweet`)".into(),
        ));
    }
    if !tweet_id.starts_with("TWEET-") {
        return Err(BatSubmitError::Gate(format!(
            "`{tweet_id}` is not a TWEET- id"
        )));
    }
    accept_surface(db_path, tweet_id, BatSurface::Tweet).await
}

enum BatSurface {
    LinkedIn,
    Tweet,
}

fn gate_accept_draft(draft_id: &str, draft: &StoredDraft) -> Result<(), BatSubmitError> {
    match draft.status.as_str() {
        status::OPEN | status::ACCEPTED => {}
        status::PUBLISHED => {
            return Err(BatSubmitError::Gate(DraftStore::gate_message(
                draft_id,
                status::PUBLISHED,
                "accept_draft",
            )));
        }
        other => {
            return Err(BatSubmitError::Gate(DraftStore::gate_message(
                draft_id,
                other,
                "accept_draft",
            )));
        }
    }
    if draft.body.trim().is_empty() {
        return Err(BatSubmitError::Gate(format!(
            "Draft `{draft_id}` has no body yet (still building?). Wait or `/draft_about` again."
        )));
    }
    Ok(())
}

async fn accept_surface(
    db_path: &Path,
    draft_id: &str,
    surface: BatSurface,
) -> Result<BatSubmitResult, BatSubmitError> {
    let draft = load_draft(db_path, draft_id)?;
    gate_accept_draft(draft_id, &draft)?;
    if let Err(reason) =
        crate::sources::publisher_url::require_ship_cite_reachable(&draft.body, &draft.link_options)
            .await
    {
        return Err(BatSubmitError::Gate(reason));
    }
    let cfg = BatGithubConfig::from_env()?;
    let owner = match surface {
        BatSurface::LinkedIn => cfg.drafts_owner.clone(),
        BatSurface::Tweet => cfg.tweet_owner.clone(),
    };
    let client = GithubClient::new(cfg)?;
    let (files, branch, title, pr_body) = match surface {
        BatSurface::LinkedIn => {
            let files = pack_draft_files(&draft);
            let branch = branch_name_for_draft(&files.draft_id);
            let title = format!("Draft: {} ({})", draft.subject, files.draft_id);
            let pr_body = draft_pr_body(&draft.subject, &files.draft_id);
            (files, branch, title, pr_body)
        }
        BatSurface::Tweet => {
            let files = pack_tweet_files(&draft);
            let branch = branch_name_for_tweet(&files.draft_id);
            let title = format!("Tweet: {} ({})", draft.subject, files.draft_id);
            let pr_body = tweet_pr_body(&draft.subject, &files.draft_id);
            (files, branch, title, pr_body)
        }
    };
    let repo_files = [
        RepoFile {
            path: files.body_path.clone(),
            content: files.body_md.clone(),
        },
        RepoFile {
            path: files.meta_path.clone(),
            content: files.meta_toml.clone(),
        },
    ];

    let existing = resolve_existing_pr(&client, &draft, &branch, &owner).await?;
    let (opened, updated_existing) = if let Some(pr) = existing {
        client
            .update_draft_pr_files(&owner, &branch, &repo_files)
            .await?;
        (pr, true)
    } else {
        let opened = match surface {
            BatSurface::LinkedIn => {
                client
                    .open_draft_pr(&branch, &title, &pr_body, &repo_files)
                    .await?
            }
            BatSurface::Tweet => {
                client
                    .open_tweet_pr(&branch, &title, &pr_body, &repo_files)
                    .await?
            }
        };
        (opened, false)
    };

    {
        let store = DraftStore::open(db_path)?;
        // From `open` → `accepted`; if already `accepted`, mark is a no-op - PR coords still refresh.
        let _ = store.mark_status(draft_id, status::ACCEPTED);
        store.set_fork_pr(draft_id, opened.number, &opened.html_url)?;
    }

    let promoted = match promote_if_approved(&client, &owner, &opened).await {
        Ok(p) => Some(p),
        Err(BatSubmitError::Gate(_)) => None,
        Err(e) => return Err(e),
    };

    Ok(BatSubmitResult {
        pr_url: opened.html_url,
        pr_number: opened.number,
        branch,
        draft_id: draft_id.to_string(),
        updated_existing,
        promoted,
    })
}

/// Ensures a draft is **open** for `/rework` / `/change_url`.
/// If status is `accepted` (fork PR waiting BAT), flips back to `open` automatically.
///
/// # Errors
///
/// Returns [`BatSubmitError`] when missing or not editable (`published` / `building` / `failed`).
pub fn ensure_open_for_edit(db_path: &Path, draft_id: &str) -> Result<StoredDraft, BatSubmitError> {
    let draft = load_draft(db_path, draft_id)?;
    match draft.status.as_str() {
        status::OPEN => Ok(draft),
        status::ACCEPTED => {
            let store = DraftStore::open(db_path)?;
            if !store.mark_status_from(draft_id, status::ACCEPTED, status::OPEN)? {
                return Err(BatSubmitError::Gate(format!(
                    "Could not unlock `{draft_id}` for edit (expected status=accepted)."
                )));
            }
            load_draft(db_path, draft_id)
        }
        other => Err(BatSubmitError::Gate(DraftStore::gate_message(
            draft_id, other, "edit",
        ))),
    }
}

/// Re-ship after BAT: missed webhook **or** promote succeeded and ship failed.
///
/// If an XPOST/Post already exists on publications, ships that body (no second merge).
/// Otherwise promotes an approved open PR, then the webhook ships.
///
/// # Errors
///
/// Returns [`BatSubmitError`] when not ready, GitHub fails, or ship fails.
pub async fn retry_bat(
    db_path: &Path,
    artefact_id: &str,
) -> Result<RetryBatResult, BatSubmitError> {
    let cfg = BatGithubConfig::from_env()?;
    let draft_id = sqlite_id_for(artefact_id);
    let owner = if draft_id.starts_with("TWEET-") {
        cfg.tweet_owner.clone()
    } else {
        cfg.drafts_owner.clone()
    };
    let client = GithubClient::new(cfg)?;
    if let Some(promoted) = client.load_promoted(artefact_id).await? {
        return ship_promoted_artefact(db_path, promoted).await;
    }
    let draft = load_draft(db_path, &draft_id)?;
    if draft.status == status::PUBLISHED {
        return Err(BatSubmitError::Gate(DraftStore::gate_message(
            &draft_id,
            status::PUBLISHED,
            "retry_bat",
        )));
    }
    if draft.status != status::ACCEPTED && draft.status != status::OPEN {
        return Err(BatSubmitError::Gate(DraftStore::gate_message(
            &draft_id,
            &draft.status,
            "retry_bat",
        )));
    }
    let branch = if draft_id.starts_with("TWEET-") {
        branch_name_for_tweet(&draft_id)
    } else {
        branch_name_for_draft(&draft_id)
    };
    let pr = resolve_existing_pr(&client, &draft, &branch, &owner)
        .await?
        .ok_or_else(|| {
            BatSubmitError::Gate(format!(
                "No open Draft PR for `{draft_id}` (branch `{branch}`) and no promoted tree to re-ship. `/accept_tweet` or `/accept_draft` first."
            ))
        })?;
    {
        let store = DraftStore::open(db_path)?;
        store.set_fork_pr(&draft_id, pr.number, &pr.html_url)?;
    }
    promote_if_approved(&client, &owner, &pr).await
}

fn sqlite_id_for(artefact_id: &str) -> String {
    crate::bat::pack::xpost_id_to_tweet_id(artefact_id)
        .or_else(|| crate::bat::pack::post_id_to_draft_id(artefact_id))
        .unwrap_or_else(|| artefact_id.to_string())
}

async fn ship_promoted_artefact(
    db_path: &Path,
    promoted: crate::bat::github::PromoteResult,
) -> Result<RetryBatResult, BatSubmitError> {
    let detail =
        if promoted.post_id.starts_with("XPOST-") || promoted.draft_id.starts_with("TWEET-") {
            ship_promoted_x(db_path, &promoted).await?
        } else {
            ship_promoted_linkedin(db_path, &promoted).await?
        };
    if let Ok(store) = DraftStore::open(db_path) {
        let _ = store.mark_status_from(&promoted.draft_id, status::ACCEPTED, status::PUBLISHED);
        let _ = store.mark_status_from(&promoted.draft_id, status::OPEN, status::PUBLISHED);
    }
    Ok(RetryBatResult {
        draft_id: promoted.draft_id,
        post_id: promoted.post_id,
        pr_number: promoted.fork_pr_number,
        detail,
    })
}

async fn ship_promoted_x(
    db_path: &Path,
    promoted: &crate::bat::github::PromoteResult,
) -> Result<String, BatSubmitError> {
    let quote = promoted.quote_tweet_id.trim();
    let request = crate::publish::XPublishRequest {
        tweet_id: Some(promoted.post_id.clone()),
        pubs_pr_number: Some(promoted.fork_pr_number).filter(|n| *n > 0),
        body: promoted.body.clone(),
        quote_tweet_id: if quote.is_empty() {
            None
        } else {
            Some(quote.to_string())
        },
        in_reply_to_tweet_id: None,
    };
    match crate::publish::ship_x_post(db_path, "playground", request, None).await {
        Ok(r) => {
            let notice = r.ship_notice_text().to_string();
            crate::slack::api::post_ship_notice(&promoted.post_id, &notice).await;
            Ok(notice)
        }
        Err(e) => {
            crate::slack::api::post_ship_fail(&promoted.post_id, &e.to_string()).await;
            Err(BatSubmitError::Gate(format!(
                "X ship failed: {}",
                crate::sources::shorten_ship_error(&e.to_string())
            )))
        }
    }
}

async fn ship_promoted_linkedin(
    db_path: &Path,
    promoted: &crate::bat::github::PromoteResult,
) -> Result<String, BatSubmitError> {
    let request = crate::publish::PublishRequest {
        draft_id: Some(promoted.post_id.clone()),
        pubs_pr_number: Some(promoted.fork_pr_number).filter(|n| *n > 0),
        body: promoted.body.clone(),
    };
    match crate::publish::ship_company_post(
        db_path,
        "playground",
        request,
        crate::publish::ShipOptions::default(),
    )
    .await
    {
        Ok(r) => {
            let notice = r.ship_notice_text().to_string();
            crate::slack::api::post_ship_notice(&promoted.post_id, &notice).await;
            Ok(notice)
        }
        Err(e) => {
            crate::slack::api::post_ship_fail(&promoted.post_id, &e.to_string()).await;
            Err(BatSubmitError::Gate(format!(
                "ship failed: {}",
                crate::sources::shorten_ship_error(&e.to_string())
            )))
        }
    }
}

async fn promote_if_approved(
    client: &GithubClient,
    drafts_owner: &str,
    pr: &OpenedPr,
) -> Result<RetryBatResult, BatSubmitError> {
    let readiness = client.bat_readiness_on(drafts_owner, pr.number).await?;
    if !readiness.approved {
        return Err(BatSubmitError::Gate(format!(
            "Draft PR #{} not BAT-ready yet (waiting Approve from {}). PR: {}",
            pr.number, readiness.expected_reviewer, pr.html_url
        )));
    }
    let promoted = client
        .promote_draft_pr_to_org(drafts_owner, pr.number)
        .await?;
    Ok(RetryBatResult {
        draft_id: promoted.draft_id,
        post_id: promoted.post_id,
        pr_number: pr.number,
        detail: format!(
            "promoted on GitHub (Draft PR #{} merged); ship runs from the webhook",
            promoted.fork_pr_number
        ),
    })
}

fn load_draft(db_path: &Path, draft_id: &str) -> Result<StoredDraft, BatSubmitError> {
    let store = DraftStore::open(db_path)?;
    store
        .get(draft_id)?
        .ok_or_else(|| BatSubmitError::NoDraft(draft_id.to_string()))
}

async fn resolve_existing_pr(
    client: &GithubClient,
    draft: &StoredDraft,
    branch: &str,
    expected_owner: &str,
) -> Result<Option<OpenedPr>, BatSubmitError> {
    if let Some(n) = draft.fork_pr_number {
        if !draft.fork_pr_url.is_empty() {
            let same_host = crate::bat::github::github_owner_from_pr_url(&draft.fork_pr_url)
                .is_some_and(|o| o.eq_ignore_ascii_case(expected_owner));
            if same_host {
                return Ok(Some(OpenedPr {
                    number: n,
                    html_url: draft.fork_pr_url.clone(),
                    branch: branch.to_string(),
                }));
            }
        }
    }
    Ok(client.find_open_pr_by_head(expected_owner, branch).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bat_submit_rejects_reply_ids() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("runtime.db");
        let err = accept_draft(&db, "CREPLY-20260825-000001")
            .await
            .expect_err("CREPLY must not BAT");
        assert!(
            format!("{err}").contains("CREPLY") || format!("{err}").contains("direct"),
            "{err}"
        );
        let err = accept_tweet(&db, "XREPLY-20260825-000001")
            .await
            .expect_err("XREPLY must not BAT");
        assert!(
            format!("{err}").contains("XREPLY") || format!("{err}").contains("direct"),
            "{err}"
        );
    }
}

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Thin GitHub REST client for publications BAT PRs (reqwest).

use base64::Engine;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;

const DEFAULT_REVIEWER: &str = "gRoussac";

#[derive(Deserialize)]
struct ContentMeta {
    sha: String,
}

#[derive(Deserialize)]
struct PullFile {
    filename: String,
    contents_url: Option<String>,
    raw_url: Option<String>,
}

#[derive(Deserialize)]
struct ContentBody {
    content: Option<String>,
    encoding: Option<String>,
}

/// Target repos for publications BAT (playground fork vs production org).
#[derive(Debug, Clone)]
pub struct BatGithubConfig {
    pub token: String,
    pub org_owner: String,
    pub fork_owner: String,
    pub repo: String,
    /// Owner for Draft PRs (fork = playground; org = production).
    pub drafts_owner: String,
    /// Draft PR base branch (always `drafts`).
    pub drafts_base: String,
    /// Owner for Post writes after BAT (same remote as drafts unless overridden).
    pub posts_owner: String,
    /// Posts branch (always `posts`).
    pub posts_base: String,
    /// Tweet PR base branch (`drafts_tweet`).
    pub tweet_drafts_base: String,
    /// Owner for Tweet PRs (fork = X playground; org = X production).
    pub tweet_owner: String,
    /// Owner for XPOST writes after BAT.
    pub tweet_posts_owner: String,
    /// Shipped tweets branch (`tweets`).
    pub tweet_posts_base: String,
    pub reviewer: String,
}

impl BatGithubConfig {
    /// Reads `GITHUB_TOKEN` from env or repo-local `.github_credentials` (KEY=value).
    ///
    /// Override path with `GITHUB_CREDS_FILE`. Same habit as `scripts/github-mcp.sh`.
    ///
    /// `LinkedIn` BAT defaults to the **fork** (`drafts` + `posts`). Playground publish mode
    /// only selects manual vs MCP ship; it does not move BAT off the fork.
    /// After fork BAT Approve, operator Approve on org **`drafts`** PR (opened at `/accept`).
    /// Org **`posts`** are not written by `ITCy` on promote.
    /// Overrides: `ITCY_BAT_DRAFTS_OWNER`, `ITCY_BAT_POSTS_OWNER`, `ITCY_BAT_DRAFTS_BASE`,
    /// `ITCY_BAT_POSTS_BASE`.
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub fn from_env() -> Result<Self, GithubError> {
        let token = load_github_token().ok_or(GithubError::MissingToken)?;
        let org_owner = env_or("ITCY_BAT_ORG_OWNER", "Interchouette-ITC");
        let fork_owner = env_or("ITCY_BAT_FORK_OWNER", "Interchouette");
        let x_playground = is_x_playground_mode();
        let tweet_default = env_or(
            "ITCY_BAT_TWEET_OWNER",
            if x_playground {
                fork_owner.as_str()
            } else {
                org_owner.as_str()
            },
        );
        let drafts_owner = env_or("ITCY_BAT_DRAFTS_OWNER", fork_owner.as_str());
        let posts_owner = env_or("ITCY_BAT_POSTS_OWNER", fork_owner.as_str());
        Ok(Self {
            token,
            org_owner,
            fork_owner,
            repo: env_or("ITCY_BAT_REPO", "itcy-publications"),
            drafts_owner,
            drafts_base: env_or("ITCY_BAT_DRAFTS_BASE", "drafts"),
            posts_owner,
            posts_base: env_or("ITCY_BAT_POSTS_BASE", "posts"),
            tweet_drafts_base: env_or("ITCY_BAT_TWEET_DRAFTS_BASE", "drafts_tweet"),
            tweet_posts_owner: env_or("ITCY_BAT_TWEET_POSTS_OWNER", &tweet_default),
            tweet_posts_base: env_or("ITCY_BAT_TWEET_POSTS_BASE", "tweets"),
            tweet_owner: tweet_default,
            reviewer: env_or("ITCY_BAT_REVIEWER", DEFAULT_REVIEWER),
        })
    }
}

/// Playground publish mode = manual `LinkedIn` ship (no CM MCP). Does not move BAT off the fork.
#[must_use]
pub fn is_playground_mode() -> bool {
    playground_flag("ITCY_BAT_PLAYGROUND").unwrap_or_else(|| {
        !matches!(
            crate::publish::resolve_publish_mode_agile("playground"),
            Ok(crate::publish::PublishMode::Production)
        )
    })
}

/// X BAT host: playground = fork, production = org. Follows X publish mode.
#[must_use]
pub fn is_x_playground_mode() -> bool {
    playground_flag("ITCY_BAT_X_PLAYGROUND").unwrap_or_else(|| {
        !matches!(
            crate::publish::resolve_x_publish_mode("playground"),
            Ok(crate::publish::PublishMode::Production)
        )
    })
}

fn playground_flag(key: &str) -> Option<bool> {
    let Ok(raw) = std::env::var(key) else {
        return None;
    };
    let v = raw.trim().to_ascii_lowercase();
    if matches!(v.as_str(), "0" | "false" | "no" | "off") {
        return Some(false);
    }
    if matches!(v.as_str(), "1" | "true" | "yes" | "on") {
        return Some(true);
    }
    None
}

/// Owner in `https://github.com/{owner}/{repo}/pull/{n}`.
#[must_use]
pub fn github_owner_from_pr_url(url: &str) -> Option<&str> {
    let rest = url.trim().strip_prefix("https://github.com/")?;
    let owner = rest.split('/').next()?;
    if owner.is_empty() {
        None
    } else {
        Some(owner)
    }
}

/// `GITHUB_TOKEN` from env or repo-local `.github_credentials` (KEY=value).
#[must_use]
pub fn github_token_from_env_or_creds() -> Option<String> {
    load_github_token()
}

fn load_github_token() -> Option<String> {
    if let Ok(t) = std::env::var("GITHUB_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    for path in github_cred_candidates() {
        if let Some(t) = token_from_key_file(&path) {
            return Some(t);
        }
    }
    None
}

fn github_cred_candidates() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("GITHUB_CREDS_FILE") {
        let p = p.trim();
        if !p.is_empty() {
            out.push(std::path::PathBuf::from(p));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(".github_credentials"));
        out.push(cwd.join("../.github_credentials"));
    }
    out.push(crate::paths::product_join(".github_credentials"));
    out
}

fn token_from_key_file(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let s = line.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        let Some((k, v)) = s.split_once('=') else {
            continue;
        };
        if k.trim() == "GITHUB_TOKEN" {
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// One file to commit via Contents API.
#[derive(Debug, Clone)]
pub struct RepoFile {
    pub path: String,
    pub content: String,
}

/// BAT webhook path after Approve on a publications PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatApproveWakeRoute {
    /// Legacy org **`drafts`** mirror: squash merge only (no ship).
    OrgDraftsMirrorMergeOnly,
    /// Fork **`posts`** / org **`tweets`** BAT: merge (if needed) + ship.
    PromoteAndShip,
    /// Not a BAT head (babysit / ignore).
    NotBat,
}

/// Result of promoting a fork Draft PR to an org Post.
#[derive(Debug, Clone)]
pub struct PromoteResult {
    pub draft_id: String,
    pub post_id: String,
    pub body: String,
    pub fork_pr_number: u64,
    /// X quote tweet id when this was a tweet BAT (empty for `LinkedIn`).
    pub quote_tweet_id: String,
    /// Tweet cite URL when this was a tweet BAT (empty for `LinkedIn`).
    pub cite: String,
}

/// Result of opening a Draft PR.
#[derive(Debug, Clone)]
pub struct OpenedPr {
    pub number: u64,
    pub html_url: String,
    pub branch: String,
}

#[derive(Deserialize)]
struct PullListItem {
    number: u64,
    html_url: String,
    head: Option<PullHeadRef>,
}

#[derive(Deserialize)]
struct PullHeadRef {
    #[serde(rename = "ref")]
    ref_: String,
}

#[derive(Debug, Error)]
pub enum GithubError {
    #[error("GITHUB_TOKEN unset (env or .github_credentials); cannot open publications BAT PR")]
    MissingToken,
    #[error("github http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("github api: {0}")]
    Api(String),
}

/// Result of closing a publications PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosePrOutcome {
    /// PR was open and is now closed.
    Closed,
    /// PR was already closed (or merged).
    AlreadyClosed,
}

#[derive(Debug, Deserialize)]
struct RefResponse {
    object: RefObject,
}

#[derive(Debug, Deserialize)]
struct RefObject {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct PullResponse {
    number: u64,
    html_url: String,
    #[serde(default)]
    state: String,
}

/// GitHub client for the BAT publications loop.
pub struct GithubClient {
    http: Client,
    cfg: BatGithubConfig,
}

impl GithubClient {
    /// Builds a client with a fixed User-Agent.
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub fn new(cfg: BatGithubConfig) -> Result<Self, GithubError> {
        let http = Client::builder()
            .user_agent("ITCy-BAT/0.1 (+https://interchouette.net)")
            .build()?;
        Ok(Self { http, cfg })
    }

    /// Creates branch from drafts base, writes `<DRAFT-id>/…` files, opens PR into **`drafts`**, requests reviewer.
    ///
    /// Legacy BAT; new `LinkedIn` BAT uses [`Self::open_post_pr`].
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn open_draft_pr(
        &self,
        branch: &str,
        title: &str,
        body: &str,
        files: &[RepoFile],
    ) -> Result<OpenedPr, GithubError> {
        let owner = self.cfg.drafts_owner.clone();
        let base = self.cfg.drafts_base.clone();
        self.open_pr_into(&owner, &base, branch, title, body, files)
            .await
    }

    /// Opens BAT PR into **`posts`** with `<POST-id>/…` files (merge on Approve; no direct branch PUT).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn open_post_pr(
        &self,
        branch: &str,
        title: &str,
        body: &str,
        files: &[RepoFile],
    ) -> Result<OpenedPr, GithubError> {
        let owner = self.cfg.posts_owner.clone();
        let base = self.cfg.posts_base.clone();
        self.open_pr_into(&owner, &base, branch, title, body, files)
            .await
    }

    /// Same as [`Self::open_draft_pr`] into **`drafts_tweet`** (legacy). New X BAT uses [`Self::open_xpost_pr`].
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn open_tweet_pr(
        &self,
        branch: &str,
        title: &str,
        body: &str,
        files: &[RepoFile],
    ) -> Result<OpenedPr, GithubError> {
        let owner = self.cfg.tweet_owner.clone();
        let base = self.cfg.tweet_drafts_base.clone();
        self.open_pr_into(&owner, &base, branch, title, body, files)
            .await
    }

    /// Opens BAT PR into **`tweets`** with `<XPOST-id>/…` files (merge on Approve).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn open_xpost_pr(
        &self,
        branch: &str,
        title: &str,
        body: &str,
        files: &[RepoFile],
    ) -> Result<OpenedPr, GithubError> {
        let owner = self.cfg.tweet_posts_owner.clone();
        let base = self.cfg.tweet_posts_base.clone();
        self.open_pr_into(&owner, &base, branch, title, body, files)
            .await
    }

    async fn open_pr_into(
        &self,
        owner: &str,
        base: &str,
        branch: &str,
        title: &str,
        body: &str,
        files: &[RepoFile],
    ) -> Result<OpenedPr, GithubError> {
        let base_sha = self.ref_sha(owner, base).await?;
        self.create_branch(owner, branch, &base_sha).await?;
        for file in files {
            self.put_file(owner, branch, file).await?;
        }
        let pr = self
            .create_pull_same_repo(owner, title, body, branch, base)
            .await?;
        self.request_reviewer_on(owner, pr.number).await?;
        Ok(OpenedPr {
            number: pr.number,
            html_url: pr.html_url,
            branch: branch.to_string(),
        })
    }

    /// Updates `<DRAFT-id>/…` files on an existing Draft head branch (same Draft PR).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn update_draft_pr_files(
        &self,
        owner: &str,
        branch: &str,
        files: &[RepoFile],
    ) -> Result<(), GithubError> {
        for file in files {
            self.put_file(owner, branch, file).await?;
        }
        Ok(())
    }

    /// Finds an open Draft PR whose head branch is `head_branch` (e.g. `draft/DRAFT-…`).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn find_open_pr_by_head(
        &self,
        owner: &str,
        head_branch: &str,
    ) -> Result<Option<OpenedPr>, GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls?state=open&per_page=50",
            owner, self.cfg.repo
        );
        let resp = self
            .http
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .header(ACCEPT, "application/vnd.github+json")
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "list pulls: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        let pulls: Vec<PullListItem> = resp.json().await?;
        for p in pulls {
            if p.head.as_ref().is_some_and(|h| h.ref_ == head_branch) {
                return Ok(Some(OpenedPr {
                    number: p.number,
                    html_url: p.html_url,
                    branch: head_branch.to_string(),
                }));
            }
        }
        Ok(None)
    }

    /// After BAT Approve: squash-merge the Post/XPOST PR, then ship.
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn promote_draft_pr_to_org(
        &self,
        pr_owner: &str,
        fork_pr_number: u64,
    ) -> Result<PromoteResult, GithubError> {
        let (body_path, body) = self
            .first_artefact_body_md(pr_owner, fork_pr_number)
            .await?
            .ok_or_else(|| {
                GithubError::Api(format!(
                    "PR #{fork_pr_number}: no POST-/DRAFT-/XPOST-/TWEET- body.md on PR"
                ))
            })?;
        if crate::bat::pack::is_xpost_body_path(&body_path) {
            self.promote_xpost_pr(pr_owner, fork_pr_number, &body_path, &body)
                .await
        } else if crate::bat::pack::is_tweet_body_path(&body_path) {
            self.promote_tweet_pr_legacy(pr_owner, fork_pr_number, &body_path, &body)
                .await
        } else if crate::bat::pack::is_post_body_path(&body_path) {
            self.promote_post_pr(pr_owner, fork_pr_number, &body_path, &body)
                .await
        } else {
            self.promote_linkedin_pr_legacy(pr_owner, fork_pr_number, &body_path, &body)
                .await
        }
    }

    async fn promote_post_pr(
        &self,
        pr_owner: &str,
        pr_number: u64,
        post_body_path: &str,
        post_body: &str,
    ) -> Result<PromoteResult, GithubError> {
        let post_id = crate::bat::pack::post_id_from_path(post_body_path).ok_or_else(|| {
            GithubError::Api(format!("cannot parse post id from path {post_body_path}"))
        })?;
        let draft_id = crate::bat::pack::post_id_to_draft_id(&post_id)
            .ok_or_else(|| GithubError::Api(format!("bad post id for promote: {post_id}")))?;
        self.merge_pull_squash_on(pr_owner, pr_number).await?;
        self.sync_org_drafts_mirror_from_post_pr(pr_owner, pr_number, &draft_id)
            .await?;
        Ok(PromoteResult {
            draft_id,
            post_id,
            body: post_body.to_string(),
            fork_pr_number: pr_number,
            quote_tweet_id: String::new(),
            cite: String::new(),
        })
    }

    async fn promote_xpost_pr(
        &self,
        pr_owner: &str,
        pr_number: u64,
        xpost_body_path: &str,
        xpost_body: &str,
    ) -> Result<PromoteResult, GithubError> {
        let xpost_id = crate::bat::pack::xpost_id_from_path(xpost_body_path).ok_or_else(|| {
            GithubError::Api(format!("cannot parse xpost id from path {xpost_body_path}"))
        })?;
        let tweet_id = crate::bat::pack::xpost_id_to_tweet_id(&xpost_id)
            .ok_or_else(|| GithubError::Api(format!("bad xpost id for promote: {xpost_id}")))?;
        let meta_path = xpost_body_path.replace("/body.md", "/meta.toml");
        let meta_raw = self
            .file_text_on_pr(pr_owner, pr_number, &meta_path)
            .await?
            .unwrap_or_default();
        let parsed = parse_pack_meta_loose(&meta_raw);
        self.merge_pull_squash_on(pr_owner, pr_number).await?;
        Ok(PromoteResult {
            draft_id: tweet_id,
            post_id: xpost_id,
            body: xpost_body.to_string(),
            fork_pr_number: pr_number,
            quote_tweet_id: parsed.quote_tweet_id,
            cite: parsed.cite,
        })
    }

    /// Legacy drafts-branch BAT: PUT onto `posts` then merge (pre posts-PR flow).
    async fn promote_linkedin_pr_legacy(
        &self,
        drafts_owner: &str,
        fork_pr_number: u64,
        draft_body_path: &str,
        draft_body: &str,
    ) -> Result<PromoteResult, GithubError> {
        let draft_id = crate::bat::pack::draft_id_from_path(draft_body_path).ok_or_else(|| {
            GithubError::Api(format!("cannot parse draft id from path {draft_body_path}"))
        })?;
        let post_id = crate::bat::pack::draft_id_to_post_id(&draft_id)
            .ok_or_else(|| GithubError::Api(format!("bad draft id for promote: {draft_id}")))?;
        let meta_path = draft_body_path.replace("/body.md", "/meta.toml");
        let meta_raw = self
            .file_text_on_pr(drafts_owner, fork_pr_number, &meta_path)
            .await?
            .unwrap_or_default();
        let parsed = parse_pack_meta_loose(&meta_raw);
        let post_body = crate::bat::pack::body_as_post(draft_body, &post_id);
        let post_meta = crate::bat::pack::pack_post_meta(&crate::bat::pack::PostMetaInput {
            draft_id: &draft_id,
            post_id: &post_id,
            subject: &parsed.subject,
            model: &parsed.model,
            tokens_in: parsed.tokens_in,
            tokens_out: parsed.tokens_out,
            sources: &parsed.sources,
            created_at: &parsed.created_at,
        });
        let (body_path, meta_path_posts) = crate::bat::pack::post_paths(&post_id);
        let posts_owner = self.cfg.posts_owner.clone();
        let posts_base = self.cfg.posts_base.clone();
        self.put_file(
            &posts_owner,
            &posts_base,
            &RepoFile {
                path: body_path,
                content: post_body.clone(),
            },
        )
        .await?;
        self.put_file(
            &posts_owner,
            &posts_base,
            &RepoFile {
                path: meta_path_posts,
                content: post_meta,
            },
        )
        .await?;
        self.merge_pull_squash_on(drafts_owner, fork_pr_number)
            .await?;
        Ok(PromoteResult {
            draft_id,
            post_id,
            body: post_body,
            fork_pr_number,
            quote_tweet_id: String::new(),
            cite: String::new(),
        })
    }

    /// True when a webhook wake targets the org **`drafts`** mirror (legacy second PR at `/accept`).
    #[must_use]
    pub fn is_org_drafts_mirror_wake(
        pr_owner: &str,
        head_ref: &str,
        cfg: &BatGithubConfig,
    ) -> bool {
        pr_owner.eq_ignore_ascii_case(&cfg.org_owner)
            && !cfg.drafts_owner.eq_ignore_ascii_case(&cfg.org_owner)
            && head_ref.starts_with("draft/")
    }

    /// Which BAT webhook path runs after Approve (pure; unit-tested).
    #[must_use]
    pub fn bat_approve_wake_route(
        pr_owner: &str,
        head_ref: &str,
        cfg: &BatGithubConfig,
    ) -> BatApproveWakeRoute {
        if Self::is_org_drafts_mirror_wake(pr_owner, head_ref, cfg) {
            return BatApproveWakeRoute::OrgDraftsMirrorMergeOnly;
        }
        if crate::bat::pack::is_bat_pr_head(head_ref) {
            return BatApproveWakeRoute::PromoteAndShip;
        }
        BatApproveWakeRoute::NotBat
    }

    /// Squash-merge an approved org **`drafts`** mirror PR only (no POST write, no ship).
    ///
    /// Fork BAT (promote + ship) must already have run on the worker fork.
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant when not BAT-ready or GitHub API fails.
    pub async fn merge_org_drafts_mirror_pr(&self, pr_number: u64) -> Result<String, GithubError> {
        let owner = self.cfg.org_owner.clone();
        let readiness = self.bat_readiness_on(&owner, pr_number).await?;
        if !readiness.approved {
            return Err(GithubError::Api(format!(
                "waiting Approve from {}",
                readiness.expected_reviewer
            )));
        }
        let head = self.pull_head_ref_on(&owner, pr_number).await?;
        if !crate::bat::pack::is_bat_pr_head(&head) {
            return Err(GithubError::Api(format!(
                "babysit non-BAT PR head `{head}` (expected post/POST-…, xpost/XPOST-…, draft/DRAFT-…, or tweet/TWEET-…)"
            )));
        }
        self.merge_pull_squash_on(&owner, pr_number).await?;
        Ok(format!(
            "org drafts mirror PR #{pr_number} merged on `{owner}/{}`",
            self.cfg.repo
        ))
    }

    /// Writes org **`drafts`** mirror files when fork BAT lands on **`posts`** (no second PR).
    async fn sync_org_drafts_mirror_from_post_pr(
        &self,
        pr_owner: &str,
        pr_number: u64,
        draft_id: &str,
    ) -> Result<(), GithubError> {
        if self
            .cfg
            .drafts_owner
            .eq_ignore_ascii_case(&self.cfg.org_owner)
        {
            return Ok(());
        }
        let post_id = crate::bat::pack::draft_id_to_post_id(draft_id).ok_or_else(|| {
            GithubError::Api(format!("bad draft id for org drafts sync: {draft_id}"))
        })?;
        let (post_body_path, post_meta_path) = crate::bat::pack::post_paths(&post_id);
        let post_body = self
            .file_text_on_pr(pr_owner, pr_number, &post_body_path)
            .await?
            .ok_or_else(|| {
                GithubError::Api(format!("PR #{pr_number}: missing {post_body_path}"))
            })?;
        let meta_raw = self
            .file_text_on_pr(pr_owner, pr_number, &post_meta_path)
            .await?
            .unwrap_or_default();
        let parsed = parse_pack_meta_loose(&meta_raw);
        let draft_body = crate::bat::pack::body_as_draft_from_post(&post_body, draft_id);
        let draft_meta = crate::bat::pack::pack_draft_meta(&crate::bat::pack::DraftMetaInput {
            draft_id,
            subject: &parsed.subject,
            model: &parsed.model,
            tokens_in: parsed.tokens_in,
            tokens_out: parsed.tokens_out,
            sources: &parsed.sources,
            created_at: &parsed.created_at,
        });
        let (body_path, meta_path) = crate::bat::pack::draft_paths(draft_id);
        let org = self.cfg.org_owner.clone();
        let base = self.cfg.drafts_base.clone();
        for file in [
            RepoFile {
                path: body_path,
                content: draft_body,
            },
            RepoFile {
                path: meta_path,
                content: draft_meta,
            },
        ] {
            self.put_file(&org, &base, &file).await?;
        }
        Ok(())
    }

    /// Sync org **`drafts`** from an on-branch POST tree (after merge or `/retry_bat`).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant when POST files are missing or PUT fails.
    pub async fn sync_org_drafts_from_posts_branch(
        &self,
        draft_id: &str,
    ) -> Result<(), GithubError> {
        if self
            .cfg
            .drafts_owner
            .eq_ignore_ascii_case(&self.cfg.org_owner)
        {
            return Ok(());
        }
        let post_id = crate::bat::pack::draft_id_to_post_id(draft_id).ok_or_else(|| {
            GithubError::Api(format!("bad draft id for org drafts sync: {draft_id}"))
        })?;
        let (post_body_path, post_meta_path) = crate::bat::pack::post_paths(&post_id);
        let posts_owner = self.cfg.posts_owner.clone();
        let posts_base = self.cfg.posts_base.clone();
        let post_body = self
            .file_text_on_branch(&posts_owner, &posts_base, &post_body_path)
            .await?
            .ok_or_else(|| {
                GithubError::Api(format!(
                    "missing {post_body_path} on {posts_owner}/{posts_base}"
                ))
            })?;
        let meta_raw = self
            .file_text_on_branch(&posts_owner, &posts_base, &post_meta_path)
            .await?
            .unwrap_or_default();
        let parsed = parse_pack_meta_loose(&meta_raw);
        let draft_body = crate::bat::pack::body_as_draft_from_post(&post_body, draft_id);
        let draft_meta = crate::bat::pack::pack_draft_meta(&crate::bat::pack::DraftMetaInput {
            draft_id,
            subject: &parsed.subject,
            model: &parsed.model,
            tokens_in: parsed.tokens_in,
            tokens_out: parsed.tokens_out,
            sources: &parsed.sources,
            created_at: &parsed.created_at,
        });
        let (body_path, meta_path) = crate::bat::pack::draft_paths(draft_id);
        let org = self.cfg.org_owner.clone();
        let base = self.cfg.drafts_base.clone();
        for file in [
            RepoFile {
                path: body_path,
                content: draft_body,
            },
            RepoFile {
                path: meta_path,
                content: draft_meta,
            },
        ] {
            self.put_file(&org, &base, &file).await?;
        }
        Ok(())
    }

    /// Opens (or refreshes) the org **`drafts`** PR for a `LinkedIn` draft (legacy; prefer [`Self::sync_org_drafts_mirror_from_post_pr`]).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn open_org_draft_pr(
        &self,
        draft_id: &str,
        draft_body: &str,
        meta_raw: &str,
        subject: &str,
    ) -> Result<Option<OpenedPr>, GithubError> {
        if self
            .cfg
            .drafts_owner
            .eq_ignore_ascii_case(&self.cfg.org_owner)
        {
            return Ok(None);
        }
        let (body_path, meta_path) = crate::bat::pack::draft_paths(draft_id);
        if self
            .file_text_on_branch(&self.cfg.org_owner, &self.cfg.drafts_base, &body_path)
            .await?
            .is_some()
        {
            return Ok(None);
        }
        let branch = crate::bat::pack::branch_name_for_draft(draft_id);
        let org = self.cfg.org_owner.clone();
        let base = self.cfg.drafts_base.clone();
        let files = [
            RepoFile {
                path: body_path,
                content: draft_body.to_string(),
            },
            RepoFile {
                path: meta_path,
                content: meta_raw.to_string(),
            },
        ];
        if let Some(existing) = self.find_open_pr_by_head(&org, &branch).await? {
            self.update_draft_pr_files(&org, &branch, &files).await?;
            return Ok(Some(existing));
        }
        let title = format!("Draft: {subject} ({draft_id})");
        let body = org_draft_pr_body(draft_id, subject);
        let pr = self
            .open_pr_into(&org, &base, &branch, &title, &body, &files)
            .await?;
        Ok(Some(pr))
    }

    async fn promote_tweet_pr_legacy(
        &self,
        drafts_owner: &str,
        fork_pr_number: u64,
        tweet_body_path: &str,
        tweet_body: &str,
    ) -> Result<PromoteResult, GithubError> {
        let tweet_id = crate::bat::pack::tweet_id_from_path(tweet_body_path).ok_or_else(|| {
            GithubError::Api(format!("cannot parse tweet id from path {tweet_body_path}"))
        })?;
        let xpost_id = crate::bat::pack::tweet_id_to_xpost_id(&tweet_id)
            .ok_or_else(|| GithubError::Api(format!("bad tweet id for promote: {tweet_id}")))?;
        let meta_path = tweet_body_path.replace("/body.md", "/meta.toml");
        let meta_raw = self
            .file_text_on_pr(drafts_owner, fork_pr_number, &meta_path)
            .await?
            .unwrap_or_default();
        let parsed = parse_pack_meta_loose(&meta_raw);
        let xpost_body = crate::bat::pack::body_as_xpost(tweet_body, &xpost_id);
        let xpost_meta = crate::bat::pack::pack_xpost_meta(&crate::bat::pack::XpostMetaInput {
            tweet_id: &tweet_id,
            xpost_id: &xpost_id,
            subject: &parsed.subject,
            model: &parsed.model,
            tokens_in: parsed.tokens_in,
            tokens_out: parsed.tokens_out,
            sources: &parsed.sources,
            created_at: &parsed.created_at,
            cite: &parsed.cite,
            quote_tweet_id: &parsed.quote_tweet_id,
        });
        let (body_path, meta_path_posts) = crate::bat::pack::xpost_paths(&xpost_id);
        let posts_owner = self.cfg.tweet_posts_owner.clone();
        let posts_base = self.cfg.tweet_posts_base.clone();
        self.put_file(
            &posts_owner,
            &posts_base,
            &RepoFile {
                path: body_path,
                content: xpost_body.clone(),
            },
        )
        .await?;
        self.put_file(
            &posts_owner,
            &posts_base,
            &RepoFile {
                path: meta_path_posts,
                content: xpost_meta,
            },
        )
        .await?;
        self.merge_pull_squash_on(drafts_owner, fork_pr_number)
            .await?;
        Ok(PromoteResult {
            draft_id: tweet_id,
            post_id: xpost_id,
            body: xpost_body,
            fork_pr_number,
            quote_tweet_id: parsed.quote_tweet_id,
            cite: parsed.cite,
        })
    }

    /// Loads an already-promoted Post or XPOST from the publications tree (ship retry).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn load_promoted(
        &self,
        artefact_id: &str,
    ) -> Result<Option<PromoteResult>, GithubError> {
        if let Some(loaded) = self.load_promoted_x(artefact_id).await? {
            return Ok(Some(loaded));
        }
        self.load_promoted_linkedin(artefact_id).await
    }

    async fn load_promoted_x(
        &self,
        artefact_id: &str,
    ) -> Result<Option<PromoteResult>, GithubError> {
        let Some((tweet_id, xpost_id)) = crate::bat::pack::tweet_xpost_ids(artefact_id) else {
            return Ok(None);
        };
        let (body_path, meta_path) = crate::bat::pack::xpost_paths(&xpost_id);
        let owner = self.cfg.tweet_posts_owner.clone();
        let branch = self.cfg.tweet_posts_base.clone();
        let Some(body) = self
            .file_text_on_branch(&owner, &branch, &body_path)
            .await?
        else {
            return Ok(None);
        };
        let meta_raw = self
            .file_text_on_branch(&owner, &branch, &meta_path)
            .await?
            .unwrap_or_default();
        let parsed = parse_pack_meta_loose(&meta_raw);
        Ok(Some(PromoteResult {
            draft_id: tweet_id,
            post_id: xpost_id,
            body,
            fork_pr_number: 0,
            quote_tweet_id: parsed.quote_tweet_id,
            cite: parsed.cite,
        }))
    }

    async fn load_promoted_linkedin(
        &self,
        artefact_id: &str,
    ) -> Result<Option<PromoteResult>, GithubError> {
        let Some((draft_id, post_id)) = crate::bat::pack::draft_post_ids(artefact_id) else {
            return Ok(None);
        };
        let (body_path, _) = crate::bat::pack::post_paths(&post_id);
        let owner = self.cfg.posts_owner.clone();
        let branch = self.cfg.posts_base.clone();
        let Some(body) = self
            .file_text_on_branch(&owner, &branch, &body_path)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(PromoteResult {
            draft_id,
            post_id,
            body,
            fork_pr_number: 0,
            quote_tweet_id: String::new(),
            cite: String::new(),
        }))
    }

    async fn file_text_on_branch(
        &self,
        owner: &str,
        branch: &str,
        path: &str,
    ) -> Result<Option<String>, GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{path}?ref={branch}",
            owner, self.cfg.repo
        );
        let resp = self
            .http
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .header(USER_AGENT, "ITCy-BAT/0.1")
            .header(ACCEPT, "application/vnd.github+json")
            .send()
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "get {path} on {owner}/{}:{branch}: {}",
                self.cfg.repo,
                resp.text().await.unwrap_or_default()
            )));
        }
        let parsed: ContentBody = resp.json().await?;
        Ok(Some(decode_content_body(parsed)?))
    }

    async fn ref_sha(&self, owner: &str, branch: &str) -> Result<String, GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/git/ref/heads/{}",
            owner, self.cfg.repo, branch
        );
        let resp = self
            .http
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .header(USER_AGENT, "ITCy-BAT/0.1")
            .send()
            .await?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            let hint = if body.contains("\"status\":\"404\"") || body.contains("Not Found") {
                format!(
                    " (branch `{branch}` missing on `{owner}/{}`? playground needs `drafts`/`posts` and `drafts_tweet`/`tweets` on the fork)",
                    self.cfg.repo
                )
            } else {
                String::new()
            };
            return Err(GithubError::Api(format!(
                "get ref {owner}/{}:{branch}: {body}{hint}",
                self.cfg.repo
            )));
        }
        let parsed: RefResponse = resp.json().await?;
        Ok(parsed.object.sha)
    }

    async fn create_branch(&self, owner: &str, branch: &str, sha: &str) -> Result<(), GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/git/refs",
            owner, self.cfg.repo
        );
        let resp = self
            .http
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .json(&json!({
                "ref": format!("refs/heads/{branch}"),
                "sha": sha,
            }))
            .send()
            .await?;
        // 422 if branch already exists: treat as ok for rework (files will update).
        if resp.status().as_u16() == 422 {
            return Ok(());
        }
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "create branch: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        Ok(())
    }

    async fn put_file(
        &self,
        owner: &str,
        branch: &str,
        file: &RepoFile,
    ) -> Result<(), GithubError> {
        const SHA_RETRY_ATTEMPTS: u32 = 3;
        let encoded_path = github_contents_path(&file.path);
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}",
            owner, self.cfg.repo, encoded_path
        );
        let b64 = base64::engine::general_purpose::STANDARD.encode(file.content.as_bytes());
        let mut last_body = String::new();
        for attempt in 0..SHA_RETRY_ATTEMPTS {
            let mut payload = json!({
                "message": format!("ITCy: {}", file.path),
                "content": b64,
                "branch": branch,
            });
            if let Some(sha) = self.existing_file_sha(owner, branch, &file.path).await? {
                payload["sha"] = json!(sha);
            }
            let resp = self
                .http
                .put(&url)
                .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
                .json(&payload)
                .send()
                .await?;
            if resp.status().is_success() {
                return Ok(());
            }
            last_body = resp.text().await.unwrap_or_default();
            if contents_put_sha_retryable(&last_body) && attempt + 1 < SHA_RETRY_ATTEMPTS {
                continue;
            }
            return Err(GithubError::Api(format_contents_put_error(
                &file.path, &last_body,
            )));
        }
        Err(GithubError::Api(format_contents_put_error(
            &file.path, &last_body,
        )))
    }

    async fn existing_file_sha(
        &self,
        owner: &str,
        branch: &str,
        path: &str,
    ) -> Result<Option<String>, GithubError> {
        let encoded_path = github_contents_path(path);
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
            owner, self.cfg.repo, encoded_path, branch
        );
        let resp = self
            .http
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .send()
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GithubError::Api(format!(
                "get contents sha {path} on {owner}/{}:{branch}: {body}",
                self.cfg.repo
            )));
        }
        let meta: ContentMeta = resp.json().await?;
        Ok(Some(meta.sha))
    }

    async fn create_pull_same_repo(
        &self,
        owner: &str,
        title: &str,
        body: &str,
        head_branch: &str,
        base: &str,
    ) -> Result<PullResponse, GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls",
            owner, self.cfg.repo
        );
        let resp = self
            .http
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .json(&json!({
                "title": title,
                "body": body,
                "head": head_branch,
                "base": base,
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "create pull: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        Ok(resp.json().await?)
    }

    async fn request_reviewer_on(&self, owner: &str, number: u64) -> Result<(), GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/requested_reviewers",
            owner, self.cfg.repo, number
        );
        let resp = self
            .http
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .json(&json!({
                "reviewers": [self.cfg.reviewer],
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "request reviewers: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        Ok(())
    }

    /// Head branch name for a publications PR (e.g. `draft/DRAFT-…`, `mig-ymd/…`).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn pull_head_ref_on(&self, owner: &str, number: u64) -> Result<String, GithubError> {
        #[derive(Deserialize)]
        struct PullHeadOnly {
            head: PullHeadRef,
        }
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{number}",
            owner, self.cfg.repo
        );
        let resp = self
            .http
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .header(ACCEPT, "application/vnd.github+json")
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "get PR #{number} head: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        let pr: PullHeadOnly = resp
            .json()
            .await
            .map_err(|e| GithubError::Api(format!("get PR #{number} head json: {e}")))?;
        Ok(pr.head.ref_)
    }

    /// Whether the configured reviewer has **Approved** the PR on `owner` (BAT green).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn bat_readiness_on(
        &self,
        owner: &str,
        number: u64,
    ) -> Result<BatReadiness, GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/reviews",
            owner, self.cfg.repo, number
        );
        let resp = self
            .http
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "list reviews: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        let reviews: Vec<ReviewRow> = resp.json().await?;
        let expected = self.cfg.reviewer.to_ascii_lowercase();
        let approved = reviews.iter().any(|r| {
            r.state.eq_ignore_ascii_case("approved")
                && r.user
                    .as_ref()
                    .is_some_and(|u| u.login.eq_ignore_ascii_case(&expected))
        });
        Ok(BatReadiness {
            approved,
            expected_reviewer: self.cfg.reviewer.clone(),
        })
    }

    /// Squash-merges a PR on `owner` (publications forbids rebase merges).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn merge_pull_squash_on(&self, owner: &str, number: u64) -> Result<(), GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/merge",
            owner, self.cfg.repo, number
        );
        let resp = self
            .http
            .put(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .json(&json!({
                "merge_method": "squash",
            }))
            .send()
            .await?;
        if resp.status().is_success() {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        if body.contains("already merged") {
            return Ok(());
        }
        Err(GithubError::Api(format!("merge PR #{number}: {body}")))
    }

    /// Close a Draft PR on the drafts owner (no-op if already closed).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn close_draft_pr(&self, number: u64) -> Result<ClosePrOutcome, GithubError> {
        let owner = self.cfg.drafts_owner.clone();
        self.close_pull_on(&owner, number).await
    }

    /// Close a Tweet PR on the tweet owner (no-op if already closed).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn close_tweet_pr(&self, number: u64) -> Result<ClosePrOutcome, GithubError> {
        let owner = self.cfg.tweet_owner.clone();
        self.close_pull_on(&owner, number).await
    }

    async fn close_pull_on(&self, owner: &str, number: u64) -> Result<ClosePrOutcome, GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{number}",
            owner, self.cfg.repo
        );
        let get = self
            .http
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .header(ACCEPT, "application/vnd.github+json")
            .send()
            .await?;
        if !get.status().is_success() {
            return Err(GithubError::Api(format!(
                "get PR #{number}: {}",
                get.text().await.unwrap_or_default()
            )));
        }
        let pr: PullResponse = get
            .json()
            .await
            .map_err(|e| GithubError::Api(format!("get PR #{number} json: {e}")))?;
        if pr.state.eq_ignore_ascii_case("closed") {
            return Ok(ClosePrOutcome::AlreadyClosed);
        }
        let resp = self
            .http
            .patch(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .header(ACCEPT, "application/vnd.github+json")
            .json(&json!({ "state": "closed" }))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "close PR #{number}: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        Ok(ClosePrOutcome::Closed)
    }

    /// First Draft or Tweet `…/body.md` on a PR.
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn first_artefact_body_md(
        &self,
        owner: &str,
        number: u64,
    ) -> Result<Option<(String, String)>, GithubError> {
        self.first_body_md_matching(owner, number, |name| {
            crate::bat::pack::is_post_body_path(name)
                || crate::bat::pack::is_draft_body_path(name)
                || crate::bat::pack::is_xpost_body_path(name)
                || crate::bat::pack::is_tweet_body_path(name)
        })
        .await
    }

    /// First Draft `…/body.md` on a PR (root `<DRAFT-id>/` or legacy `drafts/<DRAFT-id>/`).
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn first_draft_body_md(
        &self,
        owner: &str,
        number: u64,
    ) -> Result<Option<(String, String)>, GithubError> {
        self.first_body_md_matching(owner, number, |name| {
            crate::bat::pack::is_draft_body_path(name)
        })
        .await
    }

    async fn first_body_md_matching(
        &self,
        owner: &str,
        number: u64,
        pred: impl Fn(&str) -> bool,
    ) -> Result<Option<(String, String)>, GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/files?per_page=100",
            owner, self.cfg.repo, number
        );
        let resp = self
            .http
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .header(USER_AGENT, "ITCy-BAT/0.1")
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "list PR #{number} files: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        let files: Vec<PullFile> = resp.json().await?;
        let Some(file) = files.into_iter().find(|f| pred(&f.filename)) else {
            return Ok(None);
        };
        let content = if let Some(contents_url) = file.contents_url.as_deref() {
            self.fetch_contents_text(contents_url).await?
        } else if let Some(raw_url) = file.raw_url.as_deref() {
            self.fetch_raw_text(raw_url).await?
        } else {
            return Err(GithubError::Api(format!(
                "PR #{number} file {} has no contents_url/raw_url",
                file.filename
            )));
        };
        Ok(Some((file.filename, content)))
    }

    async fn file_text_on_pr(
        &self,
        owner: &str,
        number: u64,
        path: &str,
    ) -> Result<Option<String>, GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/files?per_page=100",
            owner, self.cfg.repo, number
        );
        let resp = self
            .http
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .header(USER_AGENT, "ITCy-BAT/0.1")
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let files: Vec<PullFile> = resp.json().await?;
        let Some(file) = files
            .into_iter()
            .find(|f| f.filename.replace('\\', "/") == path)
        else {
            return Ok(None);
        };
        if let Some(contents_url) = file.contents_url.as_deref() {
            return Ok(Some(self.fetch_contents_text(contents_url).await?));
        }
        if let Some(raw_url) = file.raw_url.as_deref() {
            return Ok(Some(self.fetch_raw_text(raw_url).await?));
        }
        Ok(None)
    }

    async fn fetch_contents_text(&self, contents_url: &str) -> Result<String, GithubError> {
        let resp = self
            .http
            .get(contents_url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .header(USER_AGENT, "ITCy-BAT/0.1")
            .header(ACCEPT, "application/vnd.github+json")
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "get contents: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        let parsed: ContentBody = resp.json().await?;
        decode_content_body(parsed)
    }

    async fn fetch_raw_text(&self, raw_url: &str) -> Result<String, GithubError> {
        let resp = self
            .http
            .get(raw_url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .header(USER_AGENT, "ITCy-BAT/0.1")
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "get raw: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        Ok(resp.text().await?)
    }
}

fn decode_content_body(parsed: ContentBody) -> Result<String, GithubError> {
    let raw = parsed.content.unwrap_or_default().replace('\n', "");
    if parsed
        .encoding
        .as_deref()
        .is_some_and(|e| e.eq_ignore_ascii_case("base64"))
    {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(raw.as_bytes())
            .map_err(|e| GithubError::Api(format!("decode contents: {e}")))?;
        String::from_utf8(bytes).map_err(|e| GithubError::Api(format!("utf8 contents: {e}")))
    } else {
        Ok(raw)
    }
}

/// BAT gate check for a publications PR.
#[derive(Debug, Clone)]
pub struct BatReadiness {
    pub approved: bool,
    pub expected_reviewer: String,
}

#[derive(Debug, Deserialize)]
struct ReviewRow {
    state: String,
    user: Option<ReviewUser>,
}

#[derive(Debug, Deserialize)]
struct ReviewUser {
    login: String,
}

/// True when GitHub Contents API refused a direct put because the branch requires a PR.
#[must_use]
pub fn contents_put_blocked_by_branch_protection(api_body: &str) -> bool {
    let b = api_body;
    b.contains("Changes must be made through a pull request")
        || (b.contains("\"status\":\"409\"") && b.contains("pull request"))
}

/// True when a Contents update used a stale blob SHA (409 concurrent org `drafts` sync).
#[must_use]
pub fn contents_put_sha_conflict(api_body: &str) -> bool {
    api_body.contains("is at ")
        && api_body.contains(" but expected ")
        && api_body.contains("\"status\":\"409\"")
}

/// True when Contents create ran against an existing blob (422; concurrent org `drafts` sync).
#[must_use]
pub fn contents_put_missing_sha(api_body: &str) -> bool {
    (api_body.contains("\"status\":\"422\"") || api_body.contains("\"status\":422"))
        && (api_body.contains(r#""sha" wasn't supplied"#)
            || api_body.contains(r#"\"sha\" wasn't supplied"#))
}

/// True when a Contents put should re-fetch blob SHA and retry.
#[must_use]
pub fn contents_put_sha_retryable(api_body: &str) -> bool {
    contents_put_sha_conflict(api_body) || contents_put_missing_sha(api_body)
}

/// Percent-encode a repo path segment for GitHub Contents API URLs.
fn github_contents_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            segment
                .chars()
                .map(|c| match c {
                    'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
                    _ => format!("%{:02X}", u32::from(c)),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Operator-facing Contents put error (playground `LinkedIn` BAT mirrors onto org `drafts`).
#[must_use]
pub fn format_contents_put_error(path: &str, api_body: &str) -> String {
    if contents_put_blocked_by_branch_protection(api_body) {
        format!(
            "put {path}: Contents API blocked - branch protection requires a pull request. \
             Playground LinkedIn BAT mirrors with a direct put onto org `drafts`; that branch must not require PRs."
        )
    } else if contents_put_sha_retryable(api_body) {
        format!(
            "put {path}: Contents API SHA race (concurrent org `drafts` update). \
             Retries exhausted; `/retry_bat` is safe when POST is already on `posts`."
        )
    } else {
        format!("put {path}: {api_body}")
    }
}

/// Org draft mirror targets after fork `posts` BAT (owner, branch, body path, meta path).
#[must_use]
pub fn org_drafts_mirror_targets(
    org_owner: &str,
    drafts_base: &str,
    draft_id: &str,
) -> Option<(String, String, String, String)> {
    let (body_path, meta_path) = crate::bat::pack::draft_paths(draft_id);
    if !draft_id.starts_with("DRAFT-") {
        return None;
    }
    Some((
        org_owner.to_string(),
        drafts_base.to_string(),
        body_path,
        meta_path,
    ))
}

/// PR body for a Post BAT PR (Approve = merge into **`posts`** + ship).
#[must_use]
pub fn post_pr_body(subject: &str, draft_id: &str, post_id: &str) -> String {
    let (body_path, _) = crate::bat::pack::post_paths(post_id);
    let post_dir = body_path.trim_end_matches("/body.md");
    format!(
        "## Post checklist\n\
\n\
- [x] Disclosure line present (`Written by AI - ITCy - model … - tokens in:… out:…`)\n\
- [x] Content English (or reply language matched if this is a comment reply)\n\
- [x] **gRoussac** requested as reviewer\n\
- [ ] **gRoussac Approve** = BAT = merge into **`posts`** + ship\n\
- [ ] PR comments are babysit / rework only (not BAT)\n\
\n\
## Summary\n\
\n\
Post `{post_id}` (draft `{draft_id}`) for subject `{subject}` as `{post_dir}/` on **`posts`**.\n\
\n\
## Notes\n\
\n\
Opened by ITCy. Approve merges this PR into fork **`posts`** and ships (playground = manual paste; production = MCP when set). \
At `/accept`, ITCy also opens the org **`drafts`** mirror PR on **Interchouette-ITC** (second Approve).\n"
    )
}

/// PR body for a Draft (legacy drafts-branch BAT).
#[must_use]
pub fn draft_pr_body(subject: &str, draft_id: &str) -> String {
    let (body_path, _) = crate::bat::pack::draft_paths(draft_id);
    let draft_dir = body_path.trim_end_matches("/body.md");
    format!(
        "## Draft checklist\n\
\n\
- [x] Disclosure line present (`Written by AI - ITCy - model … - tokens in:… out:…`)\n\
- [x] Content English (or reply language matched if this is a comment reply)\n\
- [x] **gRoussac** requested as reviewer\n\
- [ ] **gRoussac Approve** = BAT = only gate before Post\n\
- [ ] PR comments are babysit / rework only (not BAT)\n\
\n\
## Summary\n\
\n\
Draft `{draft_id}` for subject `{subject}` as `{draft_dir}/` on the **drafts** branch.\n\
\n\
## Notes\n\
\n\
Opened by ITCy. Fork BAT on **Interchouette** `drafts`; on Approve, POST on fork `posts` \
and ship (playground = manual paste notice; production = MCP when CM token is set). \
At `/accept`, ITCy also opens the org **`drafts`** PR on **Interchouette-ITC** (second Approve).\n"
    )
}

/// PR body for the org **`drafts`** PR (production repo, opened at `/accept`).
#[must_use]
pub fn org_draft_pr_body(draft_id: &str, subject: &str) -> String {
    let (body_path, _) = crate::bat::pack::draft_paths(draft_id);
    let draft_dir = body_path.trim_end_matches("/body.md");
    format!(
        "## Draft checklist\n\
\n\
- [x] Disclosure line present\n\
- [x] Content English\n\
- [x] **gRoussac** requested as reviewer\n\
- [ ] **gRoussac Approve** = production `drafts` mirror\n\
\n\
## Summary\n\
\n\
Draft `{draft_id}` for subject `{subject}` as `{draft_dir}/` on org **`drafts`**.\n\
\n\
Opened with the fork Draft PR at `/accept`. Fork `posts` is separate.\n"
    )
}

/// PR body for an XPOST BAT PR (Approve = merge into **`tweets`** + ship).
#[must_use]
pub fn xpost_pr_body(subject: &str, tweet_id: &str, xpost_id: &str) -> String {
    let (mode, host) = if is_x_playground_mode() {
        ("playground", "fork Interchouette")
    } else {
        ("production", "org Interchouette-ITC")
    };
    let (body_path, _) = crate::bat::pack::xpost_paths(xpost_id);
    let xpost_dir = body_path.trim_end_matches("/body.md");
    format!(
        "## XPOST checklist\n\
\n\
- [x] Disclosure line present (`Written by AI - ITCy - model … - tokens in:… out:…`)\n\
- [x] Content English\n\
- [x] **gRoussac** requested as reviewer\n\
- [ ] **gRoussac Approve** = BAT = merge into **`tweets`** + ship\n\
- [ ] PR comments are babysit / rework only (not BAT)\n\
\n\
## Summary\n\
\n\
XPOST `{xpost_id}` (tweet `{tweet_id}`) for subject `{subject}` as `{xpost_dir}/` on **`tweets`**.\n\
\n\
## Notes\n\
\n\
Opened by ITCy. X **{mode}** → {host}. Approve merges this PR into **`tweets`** and ships to X.\n"
    )
}

/// PR body for a Tweet (legacy drafts_tweet-branch BAT).
#[must_use]
pub fn tweet_pr_body(subject: &str, tweet_id: &str) -> String {
    let (mode, host) = if is_x_playground_mode() {
        ("playground", "fork Interchouette")
    } else {
        ("production", "org Interchouette-ITC")
    };
    let (body_path, _) = crate::bat::pack::tweet_paths(tweet_id);
    let tweet_dir = body_path.trim_end_matches("/body.md");
    format!(
        "## Tweet checklist\n\
\n\
- [x] Disclosure line present (`Written by AI - ITCy - model … - tokens in:… out:…`)\n\
- [x] Content English\n\
- [x] **gRoussac** requested as reviewer\n\
- [ ] **gRoussac Approve** = BAT = only gate before XPOST\n\
- [ ] PR comments are babysit / rework only (not BAT)\n\
\n\
## Summary\n\
\n\
Tweet `{tweet_id}` for subject `{subject}` as `{tweet_dir}/` on the **drafts_tweet** branch.\n\
\n\
## Notes\n\
\n\
Opened by ITCy. X **{mode}** → {host} (`drafts_tweet` + `tweets`). \
On Approve, ITCy writes a date-sharded `<XPOST-…>/` on `tweets` of that remote and ships to X.\n"
    )
}

struct PackMetaLoose {
    subject: String,
    model: String,
    tokens_in: u32,
    tokens_out: u32,
    sources: Vec<String>,
    created_at: String,
    cite: String,
    quote_tweet_id: String,
}

/// Best-effort parse of fork pack `meta.toml` (no full TOML crate dependency).
fn parse_pack_meta_loose(meta: &str) -> PackMetaLoose {
    let mut out = PackMetaLoose {
        subject: String::new(),
        model: String::new(),
        tokens_in: 0,
        tokens_out: 0,
        sources: Vec::new(),
        created_at: String::new(),
        cite: String::new(),
        quote_tweet_id: String::new(),
    };
    let mut in_sources = false;
    for line in meta.lines() {
        let t = line.trim();
        if t.starts_with("sources") && t.contains('[') {
            in_sources = true;
            if let Some(rest) = t.split_once('[').map(|(_, r)| r) {
                for part in rest.split(',') {
                    push_toml_string_token(part, &mut out.sources);
                }
            }
            if t.contains(']') {
                in_sources = false;
            }
            continue;
        }
        if in_sources {
            if t.contains(']') {
                in_sources = false;
            }
            push_toml_string_token(t, &mut out.sources);
            continue;
        }
        if let Some(v) = toml_quoted_value(t, "subject") {
            out.subject = v;
        } else if let Some(v) = toml_quoted_value(t, "model") {
            out.model = v;
        } else if let Some(v) = toml_quoted_value(t, "created_at") {
            out.created_at = v;
        } else if let Some(v) = toml_quoted_value(t, "cite") {
            out.cite = v;
        } else if let Some(v) = toml_quoted_value(t, "quote_tweet_id") {
            out.quote_tweet_id = v;
        } else if let Some(v) = toml_int_value(t, "tokens_in") {
            out.tokens_in = v;
        } else if let Some(v) = toml_int_value(t, "tokens_out") {
            out.tokens_out = v;
        }
    }
    out
}

fn toml_quoted_value(line: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} =");
    let rest = line.strip_prefix(&prefix)?.trim();
    let rest = rest.strip_prefix('"')?.strip_suffix('"')?;
    Some(rest.replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn toml_int_value(line: &str, key: &str) -> Option<u32> {
    let prefix = format!("{key} =");
    let rest = line.strip_prefix(&prefix)?.trim();
    rest.parse().ok()
}

fn push_toml_string_token(part: &str, out: &mut Vec<String>) {
    let t = part.trim().trim_end_matches(',').trim();
    if let Some(inner) = t.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        if !inner.is_empty() {
            out.push(inner.replace("\\\"", "\"").replace("\\\\", "\\"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_draft_pr_body_mentions_production_drafts() {
        let b = org_draft_pr_body("DRAFT-20260801-000001", "subject");
        assert!(b.contains("org **`drafts`**"));
        assert!(b.contains("2026/08/01/DRAFT-20260801-000001/"));
    }

    #[test]
    fn org_drafts_mirror_targets_org_drafts_branch_not_posts() {
        // POST-132 failure: sync must put DRAFT paths on org `drafts`, not fork `posts`.
        let (owner, branch, body, meta) =
            org_drafts_mirror_targets("Interchouette-ITC", "drafts", "DRAFT-20260829-000132")
                .expect("draft id");
        assert_eq!(owner, "Interchouette-ITC");
        assert_eq!(branch, "drafts");
        assert_eq!(body, "2026/08/29/DRAFT-20260829-000132/body.md");
        assert_eq!(meta, "2026/08/29/DRAFT-20260829-000132/meta.toml");
        assert!(org_drafts_mirror_targets("Interchouette-ITC", "drafts", "POST-1").is_none());
    }

    #[test]
    fn contents_put_error_names_branch_protection_not_generic_409() {
        // Live failure on DRAFT-20260829-000132 before org `drafts` was unprotected.
        let api = r#"{"message":"Could not create file: Changes must be made through a pull request.","documentation_url":"https://docs.github.com/articles/about-protected-branches","status":"409"}"#;
        assert!(contents_put_blocked_by_branch_protection(api));
        let msg = format_contents_put_error("2026/08/29/DRAFT-20260829-000132/body.md", api);
        assert!(
            msg.contains("branch protection requires a pull request"),
            "{msg}"
        );
        assert!(msg.contains("org `drafts`"), "{msg}");
        assert!(!contents_put_blocked_by_branch_protection(
            "put failed: 500 boom"
        ));
        assert_eq!(format_contents_put_error("x.md", "nope"), "put x.md: nope");
    }

    #[test]
    fn contents_put_sha_conflict_detects_stale_blob_409() {
        // DRAFT-20260901-000138: org `drafts` mirror PUT raced; merge ok, sync aborted BAT.
        let api = r#"{"message":"is at 5fd8c11ecd6e55466742d8ea626b83915e28e5f2 but expected fb3383a4729939701700d5fd2c31928b45c16a8d","documentation_url":"https://docs.github.com/rest/repos/contents#create-or-update-file-contents","status":"409"}"#;
        assert!(contents_put_sha_conflict(api));
        assert!(!contents_put_missing_sha(api));
        assert!(contents_put_sha_retryable(api));
        assert!(!contents_put_blocked_by_branch_protection(api));
        let msg = format_contents_put_error("2026/09/01/DRAFT-20260901-000138/body.md", api);
        assert!(msg.contains("SHA race"), "{msg}");
        assert!(msg.contains("/retry_bat"), "{msg}");
    }

    #[test]
    fn contents_put_missing_sha_detects_existing_blob_422() {
        // DRAFT-20260901-000141: org `drafts` meta PUT raced (create without sha on existing file).
        let api = r#"{"message":"Invalid request.\n\n\"sha\" wasn't supplied.","documentation_url":"https://docs.github.com/rest/repos/contents#create-or-update-file-contents","status":"422"}"#;
        assert!(contents_put_missing_sha(api));
        assert!(!contents_put_sha_conflict(api));
        assert!(contents_put_sha_retryable(api));
        let msg = format_contents_put_error("2026/09/01/DRAFT-20260901-000141/meta.toml", api);
        assert!(msg.contains("SHA race"), "{msg}");
        assert!(msg.contains("/retry_bat"), "{msg}");
    }

    #[test]
    fn github_contents_path_encodes_segments() {
        assert_eq!(
            github_contents_path("2026/09/01/DRAFT-20260901-000141/meta.toml"),
            "2026/09/01/DRAFT-20260901-000141/meta.toml"
        );
        assert_eq!(github_contents_path("a b/c"), "a%20b/c");
    }

    #[test]
    fn linkedin_bat_defaults_to_fork_owners() {
        let _guard = env_lock().lock().expect("env lock");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("creds");
        std::fs::write(&path, "GITHUB_TOKEN=ghp_test\n").expect("write");
        unsafe {
            std::env::remove_var("ITCY_BAT_DRAFTS_OWNER");
            std::env::remove_var("ITCY_BAT_POSTS_OWNER");
            std::env::remove_var("ITCY_BAT_OWNER");
            std::env::set_var("GITHUB_CREDS_FILE", &path);
            std::env::set_var("ITCY_LINKEDIN_PUBLISH_MODE", "production");
        }
        let cfg = BatGithubConfig::from_env().expect("cfg");
        assert_eq!(cfg.drafts_owner, "Interchouette");
        assert_eq!(cfg.posts_owner, "Interchouette");
        unsafe {
            std::env::remove_var("GITHUB_CREDS_FILE");
            std::env::remove_var("ITCY_LINKEDIN_PUBLISH_MODE");
        }
    }

    #[test]
    fn pr_body_mentions_draft_and_bat() {
        let b = draft_pr_body("rust", "DRAFT-20260801-000001");
        assert!(b.contains("gRoussac"));
        assert!(b.contains("Approve"));
        assert!(b.contains("2026/08/01/DRAFT-20260801-000001/"));
        assert!(b.contains("BAT"));
        assert!(b.contains("playground"));
        assert!(b.contains("posts"));
        assert!(!b.contains("publications"));
    }

    #[test]
    fn tweet_pr_body_follows_x_mode_host() {
        let _guard = env_lock().lock().expect("env lock");
        // SAFETY: serialized by env_lock; restored before unlock.
        unsafe {
            std::env::remove_var("ITCY_BAT_X_PLAYGROUND");
            std::env::set_var("ITCY_X_PUBLISH_MODE", "production");
        }
        let prod = tweet_pr_body("rust", "TWEET-20260801-000001");
        assert!(prod.contains("production"));
        assert!(prod.contains("org Interchouette-ITC"));
        assert!(prod.contains("`2026/08/01/TWEET-20260801-000001/`"));
        unsafe {
            std::env::set_var("ITCY_X_PUBLISH_MODE", "playground");
        }
        let play = tweet_pr_body("rust", "TWEET-20260801-000001");
        assert!(play.contains("playground"));
        assert!(play.contains("fork Interchouette"));
        assert!(!play.contains("Interchouette-ITC"));
        unsafe {
            std::env::remove_var("ITCY_X_PUBLISH_MODE");
        }
    }

    #[test]
    fn github_owner_from_pr_url_parses() {
        assert_eq!(
            github_owner_from_pr_url("https://github.com/Interchouette/itcy-publications/pull/5"),
            Some("Interchouette")
        );
        assert_eq!(
            github_owner_from_pr_url(
                "https://github.com/Interchouette-ITC/itcy-publications/pull/1"
            ),
            Some("Interchouette-ITC")
        );
    }

    #[test]
    fn org_drafts_mirror_wake_only_on_org_when_fork_bat() {
        let cfg = fork_playground_bat_cfg();
        assert!(GithubClient::is_org_drafts_mirror_wake(
            "Interchouette-ITC",
            "draft/DRAFT-20260801-000001",
            &cfg
        ));
        assert!(!GithubClient::is_org_drafts_mirror_wake(
            "Interchouette-ITC",
            "xpost/XPOST-20260828-000093",
            &cfg
        ));
        assert!(!GithubClient::is_org_drafts_mirror_wake(
            "Interchouette",
            "draft/DRAFT-20260801-000001",
            &cfg
        ));
    }

    /// Regression: org production X BAT (PR #105 / TWEET-20260828-000093) must promote+ship, not mirror merge.
    #[test]
    fn bat_approve_wake_route_matrix() {
        let fork = fork_playground_bat_cfg();
        let prod_x = production_x_bat_cfg();
        let cases: &[(&str, &str, &BatGithubConfig, BatApproveWakeRoute, &str)] = &[
            (
                "Interchouette-ITC",
                "draft/DRAFT-20260801-000001",
                &fork,
                BatApproveWakeRoute::OrgDraftsMirrorMergeOnly,
                "legacy org drafts mirror",
            ),
            (
                "Interchouette-ITC",
                "xpost/XPOST-20260828-000093",
                &prod_x,
                BatApproveWakeRoute::PromoteAndShip,
                "regression org production tweet BAT PR #105",
            ),
            (
                "Interchouette-ITC",
                "post/POST-20260828-000129",
                &fork,
                BatApproveWakeRoute::PromoteAndShip,
                "org post head is never mirror-only",
            ),
            (
                "Interchouette",
                "post/POST-20260828-000129",
                &fork,
                BatApproveWakeRoute::PromoteAndShip,
                "fork linkedin posts BAT",
            ),
            (
                "Interchouette",
                "xpost/XPOST-20260828-000065",
                &fork,
                BatApproveWakeRoute::PromoteAndShip,
                "fork x playground BAT",
            ),
            (
                "Interchouette",
                "draft/DRAFT-20260801-000001",
                &fork,
                BatApproveWakeRoute::PromoteAndShip,
                "fork linkedin draft branch BAT",
            ),
            (
                "Interchouette",
                "tweet/TWEET-20260828-000093",
                &fork,
                BatApproveWakeRoute::PromoteAndShip,
                "legacy tweet head on fork",
            ),
            (
                "Interchouette-ITC",
                "mig-ymd/itcy-drafts",
                &fork,
                BatApproveWakeRoute::NotBat,
                "migration PR babysit",
            ),
        ];
        for (owner, head, cfg, want, label) in cases {
            let got = GithubClient::bat_approve_wake_route(owner, head, cfg);
            assert_eq!(got, *want, "{label}: owner={owner} head={head}");
        }
    }

    fn fork_playground_bat_cfg() -> BatGithubConfig {
        BatGithubConfig {
            token: "t".into(),
            org_owner: "Interchouette-ITC".into(),
            fork_owner: "Interchouette".into(),
            repo: "itcy-publications".into(),
            drafts_owner: "Interchouette".into(),
            posts_owner: "Interchouette".into(),
            drafts_base: "drafts".into(),
            posts_base: "posts".into(),
            tweet_drafts_base: "drafts_tweet".into(),
            tweet_owner: "Interchouette".into(),
            tweet_posts_owner: "Interchouette".into(),
            tweet_posts_base: "tweets".into(),
            reviewer: "gRoussac".into(),
        }
    }

    fn production_x_bat_cfg() -> BatGithubConfig {
        BatGithubConfig {
            tweet_owner: "Interchouette-ITC".into(),
            tweet_posts_owner: "Interchouette-ITC".into(),
            ..fork_playground_bat_cfg()
        }
    }

    #[test]
    fn playground_mode_follows_publish_mode_and_override() {
        let _guard = env_lock().lock().expect("env lock");
        // SAFETY: serialized by env_lock; restored before unlock.
        // Drive only via env - do not depend on committed config.toml defaults.
        unsafe {
            std::env::remove_var("ITCY_BAT_PLAYGROUND");
            std::env::remove_var("ITCY_BAT_X_PLAYGROUND");
            std::env::set_var("ITCY_LINKEDIN_PUBLISH_MODE", "playground");
            std::env::set_var("ITCY_X_PUBLISH_MODE", "production");
        }
        assert!(is_playground_mode());
        assert!(!is_x_playground_mode());
        unsafe {
            std::env::set_var("ITCY_LINKEDIN_PUBLISH_MODE", "production");
        }
        assert!(!is_playground_mode());
        unsafe {
            std::env::set_var("ITCY_BAT_PLAYGROUND", "true");
        }
        assert!(is_playground_mode());
        unsafe {
            std::env::remove_var("ITCY_BAT_PLAYGROUND");
            std::env::remove_var("ITCY_LINKEDIN_PUBLISH_MODE");
            std::env::remove_var("ITCY_X_PUBLISH_MODE");
            std::env::remove_var("ITCY_BAT_X_PLAYGROUND");
        }
    }

    #[test]
    fn load_token_from_github_creds_file() {
        let _guard = env_lock().lock().expect("env lock");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("creds");
        std::fs::write(&path, "GITHUB_TOKEN=ghp_test_token_for_unit\n").expect("write");
        // SAFETY: serialized by env_lock; restored before unlock.
        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
            std::env::set_var("GITHUB_CREDS_FILE", &path);
        }
        let got = load_github_token();
        unsafe {
            std::env::remove_var("GITHUB_CREDS_FILE");
        }
        assert_eq!(got.as_deref(), Some("ghp_test_token_for_unit"));
    }

    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }
}

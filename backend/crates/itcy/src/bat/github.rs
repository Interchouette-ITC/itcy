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
    /// Soft debug = playground (mock): fork `drafts` + `posts`.
    /// Live/cutover = production: org `drafts` + `posts`.
    /// Same branch names; remote selects playground vs real.
    /// Overrides: `ITCY_BAT_OWNER`, `ITCY_BAT_DRAFTS_OWNER`, `ITCY_BAT_POSTS_OWNER`,
    /// `ITCY_BAT_DRAFTS_BASE`, `ITCY_BAT_POSTS_BASE`.
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub fn from_env() -> Result<Self, GithubError> {
        let token = load_github_token().ok_or(GithubError::MissingToken)?;
        let org_owner = env_or("ITCY_BAT_ORG_OWNER", "Interchouette-ITC");
        let fork_owner = env_or("ITCY_BAT_FORK_OWNER", "Interchouette");
        let playground = is_playground_mode();
        let x_playground = is_x_playground_mode();
        let owner = env_or(
            "ITCY_BAT_OWNER",
            if playground {
                fork_owner.as_str()
            } else {
                org_owner.as_str()
            },
        );
        let tweet_default = env_or(
            "ITCY_BAT_TWEET_OWNER",
            if x_playground {
                fork_owner.as_str()
            } else {
                org_owner.as_str()
            },
        );
        Ok(Self {
            token,
            org_owner,
            fork_owner,
            repo: env_or("ITCY_BAT_REPO", "itcy-publications"),
            drafts_owner: env_or("ITCY_BAT_DRAFTS_OWNER", &owner),
            drafts_base: env_or("ITCY_BAT_DRAFTS_BASE", "drafts"),
            posts_owner: env_or("ITCY_BAT_POSTS_OWNER", &owner),
            posts_base: env_or("ITCY_BAT_POSTS_BASE", "posts"),
            tweet_drafts_base: env_or("ITCY_BAT_TWEET_DRAFTS_BASE", "drafts_tweet"),
            tweet_posts_owner: env_or("ITCY_BAT_TWEET_POSTS_OWNER", &tweet_default),
            tweet_posts_base: env_or("ITCY_BAT_TWEET_POSTS_BASE", "tweets"),
            tweet_owner: tweet_default,
            reviewer: env_or("ITCY_BAT_REVIEWER", DEFAULT_REVIEWER),
        })
    }
}

/// Soft debug / mock = playground (fork). Live company-page mode = production org cutover.
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

    /// Same as [`Self::open_draft_pr`] into **`drafts_tweet`**.
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

    /// After BAT Approve: write Post on `posts` or XPOST on `tweets`, then merge the PR.
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn promote_draft_pr_to_org(
        &self,
        pr_owner: &str,
        fork_pr_number: u64,
    ) -> Result<PromoteResult, GithubError> {
        let (body_path, draft_body) = self
            .first_artefact_body_md(pr_owner, fork_pr_number)
            .await?
            .ok_or_else(|| {
                GithubError::Api(format!(
                    "PR #{fork_pr_number}: no DRAFT-*/body.md or TWEET-*/body.md on PR"
                ))
            })?;
        if crate::bat::pack::is_tweet_body_path(&body_path) {
            self.promote_tweet_pr(pr_owner, fork_pr_number, &body_path, &draft_body)
                .await
        } else {
            self.promote_linkedin_pr(pr_owner, fork_pr_number, &body_path, &draft_body)
                .await
        }
    }

    async fn promote_linkedin_pr(
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
        self.merge_pull_rebase_on(drafts_owner, fork_pr_number)
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

    async fn promote_tweet_pr(
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
        self.merge_pull_rebase_on(drafts_owner, fork_pr_number)
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
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}",
            owner, self.cfg.repo, file.path
        );
        let b64 = base64::engine::general_purpose::STANDARD.encode(file.content.as_bytes());
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
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "put {}: {}",
                file.path,
                resp.text().await.unwrap_or_default()
            )));
        }
        Ok(())
    }

    async fn existing_file_sha(
        &self,
        owner: &str,
        branch: &str,
        path: &str,
    ) -> Result<Option<String>, GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
            owner, self.cfg.repo, path, branch
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
            return Ok(None);
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

    /// Rebase-merges a PR on `owner`.
    ///
    /// # Errors
    ///
    /// Returns a [`GithubError`] variant for HTTP, auth, or GitHub API failure.
    pub async fn merge_pull_rebase_on(&self, owner: &str, number: u64) -> Result<(), GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/merge",
            owner, self.cfg.repo, number
        );
        let resp = self
            .http
            .put(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.cfg.token))
            .json(&json!({
                "merge_method": "rebase",
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(GithubError::Api(format!(
                "merge PR #{number}: {}",
                resp.text().await.unwrap_or_default()
            )));
        }
        Ok(())
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
            crate::bat::pack::is_draft_body_path(name) || crate::bat::pack::is_tweet_body_path(name)
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

/// PR body for a Draft (Approve = BAT → Post on `posts` of the active remote).
#[must_use]
pub fn draft_pr_body(subject: &str, draft_id: &str) -> String {
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
Draft `{draft_id}` for subject `{subject}` as `{draft_id}/` on the **drafts** branch.\n\
\n\
## Notes\n\
\n\
Opened by ITCy. Soft debug = **playground** (fork Interchouette: `drafts` + `posts`). \
Production = org Interchouette-ITC: same branch names (`drafts` + `posts`, real artefacts). \
On Approve, ITCy writes `<POST-…>/` on `posts` of the active remote and ships. \
Live needs CM token.\n"
    )
}

/// PR body for a Tweet (Approve = BAT → XPOST on `tweets`).
#[must_use]
pub fn tweet_pr_body(subject: &str, tweet_id: &str) -> String {
    let (mode, host) = if is_x_playground_mode() {
        ("playground", "fork Interchouette")
    } else {
        ("production", "org Interchouette-ITC")
    };
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
Tweet `{tweet_id}` for subject `{subject}` as `{tweet_id}/` on the **drafts_tweet** branch.\n\
\n\
## Notes\n\
\n\
Opened by ITCy. X **{mode}** → {host} (`drafts_tweet` + `tweets`). \
On Approve, ITCy writes `<XPOST-…>/` on that remote and ships to X.\n"
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
    fn pr_body_mentions_draft_and_bat() {
        let b = draft_pr_body("rust", "DRAFT-20260801-000001");
        assert!(b.contains("gRoussac"));
        assert!(b.contains("Approve"));
        assert!(b.contains("DRAFT-20260801-000001/"));
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
    fn playground_mode_follows_publish_mode_and_override() {
        let _guard = env_lock().lock().expect("env lock");
        // SAFETY: serialized by env_lock; restored before unlock.
        unsafe {
            std::env::remove_var("ITCY_BAT_PLAYGROUND");
            std::env::remove_var("ITCY_LINKEDIN_PUBLISH_MODE");
            std::env::remove_var("ITCY_BAT_X_PLAYGROUND");
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

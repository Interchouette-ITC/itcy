// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Pack an open draft into `YYYY/MM/DD/<DRAFT-id>/` on the drafts branch; promote helpers for posts.

use crate::bat::store::PendingDraft;
use chrono::Local;
use std::fmt::Write;

/// Files written under `YYYY/MM/DD/<DRAFT-id>/` on the drafts branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftFiles {
    /// Draft id (`DRAFT-YYYYMMDD-NNNNNN`).
    pub draft_id: String,
    pub body_path: String,
    pub meta_path: String,
    pub body_md: String,
    pub meta_toml: String,
}

/// Feature branch for a Draft PR (head); base is the `drafts` branch.
#[must_use]
pub fn branch_name_for_draft(draft_id: &str) -> String {
    format!("draft/{draft_id}")
}

/// `DRAFT-YYYYMMDD-NNNNNN` → `POST-YYYYMMDD-NNNNNN`.
#[must_use]
pub fn draft_id_to_post_id(draft_id: &str) -> Option<String> {
    draft_id
        .strip_prefix("DRAFT-")
        .filter(|rest| !rest.is_empty())
        .map(|rest| format!("POST-{rest}"))
}

/// Paths for a Post on the `posts` branch.
#[must_use]
pub fn post_paths(post_id: &str) -> (String, String) {
    sharded_artefact_paths(post_id, "POST-")
}

/// Paths for a Draft on `drafts`.
#[must_use]
pub fn draft_paths(draft_id: &str) -> (String, String) {
    sharded_artefact_paths(draft_id, "DRAFT-")
}

/// Rewrite Draft body header to Post ID for Post `body.md`.
#[must_use]
pub fn body_as_post(body: &str, post_id: &str) -> String {
    rewrite_id_header(body, "Draft ID:", "Post ID:", post_id, "DRAFT-")
}

/// Feature branch for a Tweet PR (head); base is `drafts_tweet`.
#[must_use]
pub fn branch_name_for_tweet(tweet_id: &str) -> String {
    format!("tweet/{tweet_id}")
}

/// `TWEET-YYYYMMDD-NNNNNN` → `XPOST-YYYYMMDD-NNNNNN`.
#[must_use]
pub fn tweet_id_to_xpost_id(tweet_id: &str) -> Option<String> {
    tweet_id
        .strip_prefix("TWEET-")
        .filter(|rest| !rest.is_empty())
        .map(|rest| format!("XPOST-{rest}"))
}

/// `XPOST-YYYYMMDD-NNNNNN` → `TWEET-YYYYMMDD-NNNNNN`.
#[must_use]
pub fn xpost_id_to_tweet_id(xpost_id: &str) -> Option<String> {
    xpost_id
        .strip_prefix("XPOST-")
        .filter(|rest| !rest.is_empty())
        .map(|rest| format!("TWEET-{rest}"))
}

/// `POST-YYYYMMDD-NNNNNN` → `DRAFT-YYYYMMDD-NNNNNN`.
#[must_use]
pub fn post_id_to_draft_id(post_id: &str) -> Option<String> {
    post_id
        .strip_prefix("POST-")
        .filter(|rest| !rest.is_empty())
        .map(|rest| format!("DRAFT-{rest}"))
}

/// Tweet + XPOST ids from either prefix.
#[must_use]
pub fn tweet_xpost_ids(id: &str) -> Option<(String, String)> {
    if let Some(xpost) = tweet_id_to_xpost_id(id) {
        return Some((id.to_string(), xpost));
    }
    let tweet = xpost_id_to_tweet_id(id)?;
    Some((tweet, id.to_string()))
}

/// Draft + Post ids from either prefix.
#[must_use]
pub fn draft_post_ids(id: &str) -> Option<(String, String)> {
    if let Some(post) = draft_id_to_post_id(id) {
        return Some((id.to_string(), post));
    }
    let draft = post_id_to_draft_id(id)?;
    Some((draft, id.to_string()))
}

/// Paths for an XPOST on `tweets`.
#[must_use]
pub fn xpost_paths(xpost_id: &str) -> (String, String) {
    sharded_artefact_paths(xpost_id, "XPOST-")
}

/// Rewrite Tweet body header to XPOST ID.
#[must_use]
pub fn body_as_xpost(body: &str, xpost_id: &str) -> String {
    rewrite_id_header(body, "Tweet ID:", "XPOST ID:", xpost_id, "TWEET-")
}

fn rewrite_id_header(
    body: &str,
    from_label: &str,
    to_label: &str,
    new_id: &str,
    from_prefix: &str,
) -> String {
    let mut lines = body.lines();
    let mut out = String::new();
    if let Some(first) = lines.next() {
        let t = first.trim();
        if t.starts_with(from_label) || t.starts_with(from_prefix) {
            let _ = writeln!(out, "{to_label} {new_id}");
        } else {
            let _ = write!(out, "{to_label} {new_id}\n\n");
            out.push_str(first);
            out.push('\n');
        }
    } else {
        let _ = writeln!(out, "{to_label} {new_id}");
    }
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Builds `body.md` + `meta.toml` for `YYYY/MM/DD/<DRAFT-id>/` on the drafts branch.
#[must_use]
pub fn pack_draft_files(draft: &PendingDraft) -> DraftFiles {
    let draft_id = draft.draft_id.clone();
    let created = if draft.created_at.is_empty() {
        Local::now().to_rfc3339()
    } else {
        draft.created_at.clone()
    };
    let mut sources_toml = String::from("[\n");
    for s in &draft.sources {
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = writeln!(sources_toml, "  \"{escaped}\",");
    }
    sources_toml.push(']');
    let meta_toml = format!(
        "kind = \"draft\"\n\
draft_id = {draft_id}\n\
subject = {subject}\n\
created_at = \"{created}\"\n\
model = \"{model}\"\n\
tokens_in = {tin}\n\
tokens_out = {tout}\n\
sources = {sources_toml}\n",
        draft_id = toml_string(&draft_id),
        subject = toml_string(&draft.subject),
        model = draft.model.replace('"', ""),
        tin = draft.tokens_in,
        tout = draft.tokens_out,
    );
    let (body_path, meta_path) = draft_paths(&draft_id);
    DraftFiles {
        body_path,
        meta_path,
        body_md: draft.body.clone(),
        meta_toml,
        draft_id,
    }
}

/// Inputs for Post `meta.toml` after promote.
#[derive(Debug, Clone)]
pub struct PostMetaInput<'a> {
    pub draft_id: &'a str,
    pub post_id: &'a str,
    pub subject: &'a str,
    pub model: &'a str,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub sources: &'a [String],
    pub created_at: &'a str,
}

/// Post `meta.toml` after promote (`kind = post`).
#[must_use]
pub fn pack_post_meta(input: &PostMetaInput<'_>) -> String {
    let created = if input.created_at.is_empty() {
        Local::now().to_rfc3339()
    } else {
        input.created_at.to_string()
    };
    let mut sources_toml = String::from("[\n");
    for s in input.sources {
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = writeln!(sources_toml, "  \"{escaped}\",");
    }
    sources_toml.push(']');
    format!(
        "kind = \"post\"\n\
post_id = {post_id}\n\
draft_id = {draft_id}\n\
subject = {subject}\n\
created_at = \"{created}\"\n\
model = \"{model}\"\n\
tokens_in = {tin}\n\
tokens_out = {tout}\n\
sources = {sources_toml}\n",
        post_id = toml_string(input.post_id),
        draft_id = toml_string(input.draft_id),
        subject = toml_string(input.subject),
        model = input.model.replace('"', ""),
        tin = input.tokens_in,
        tout = input.tokens_out,
    )
}

/// Inputs for Tweet `meta.toml` (`kind = tweet`).
#[derive(Debug, Clone)]
pub struct TweetMetaInput<'a> {
    pub tweet_id: &'a str,
    pub subject: &'a str,
    pub model: &'a str,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub sources: &'a [String],
    pub created_at: &'a str,
    pub cite: &'a str,
    pub quote_tweet_id: &'a str,
}

/// Builds `body.md` + `meta.toml` for `<TWEET-id>/` on `drafts_tweet`.
#[must_use]
pub fn pack_tweet_files(draft: &PendingDraft) -> DraftFiles {
    let tweet_id = draft.draft_id.clone();
    let created = if draft.created_at.is_empty() {
        Local::now().to_rfc3339()
    } else {
        draft.created_at.clone()
    };
    let cite = crate::sources::tweet_footer::primary_cite(&draft.link_options)
        .unwrap_or("")
        .to_string();
    let quote = crate::sources::tweet_footer::quote_tweet_id_from_cites(&draft.link_options)
        .unwrap_or_default();
    let meta_toml = pack_tweet_meta(&TweetMetaInput {
        tweet_id: &tweet_id,
        subject: &draft.subject,
        model: &draft.model,
        tokens_in: draft.tokens_in,
        tokens_out: draft.tokens_out,
        sources: &draft.sources,
        created_at: &created,
        cite: &cite,
        quote_tweet_id: &quote,
    });
    let (body_path, meta_path) = tweet_paths(&tweet_id);
    DraftFiles {
        body_path,
        meta_path,
        body_md: draft.body.clone(),
        meta_toml,
        draft_id: tweet_id,
    }
}

/// Paths for a Tweet on `drafts_tweet`.
#[must_use]
pub fn tweet_paths(tweet_id: &str) -> (String, String) {
    sharded_artefact_paths(tweet_id, "TWEET-")
}

fn sharded_artefact_paths(id: &str, prefix: &str) -> (String, String) {
    shard_prefix_from_id(id, prefix).map_or_else(
        || (format!("{id}/body.md"), format!("{id}/meta.toml")),
        |shard| {
            (
                format!("{shard}/{id}/body.md"),
                format!("{shard}/{id}/meta.toml"),
            )
        },
    )
}

fn shard_prefix_from_id(id: &str, prefix: &str) -> Option<String> {
    let rest = id.strip_prefix(prefix)?;
    let date = rest.split('-').next()?;
    if date.len() < 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("{}/{}/{}", &date[0..4], &date[4..6], &date[6..8]))
}

/// Tweet `meta.toml` (`kind = tweet`).
#[must_use]
pub fn pack_tweet_meta(input: &TweetMetaInput<'_>) -> String {
    let created = if input.created_at.is_empty() {
        Local::now().to_rfc3339()
    } else {
        input.created_at.to_string()
    };
    format!(
        "kind = \"tweet\"\n\
tweet_id = {tweet_id}\n\
subject = {subject}\n\
created_at = \"{created}\"\n\
model = \"{model}\"\n\
tokens_in = {tin}\n\
tokens_out = {tout}\n\
cite = {cite}\n\
quote_tweet_id = {quote}\n\
sources = {sources}\n",
        tweet_id = toml_string(input.tweet_id),
        subject = toml_string(input.subject),
        model = input.model.replace('"', ""),
        tin = input.tokens_in,
        tout = input.tokens_out,
        cite = toml_string(input.cite),
        quote = toml_string(input.quote_tweet_id),
        sources = sources_toml_list(input.sources),
    )
}

/// Inputs for XPOST `meta.toml` after promote.
#[derive(Debug, Clone)]
pub struct XpostMetaInput<'a> {
    pub tweet_id: &'a str,
    pub xpost_id: &'a str,
    pub subject: &'a str,
    pub model: &'a str,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub sources: &'a [String],
    pub created_at: &'a str,
    pub cite: &'a str,
    pub quote_tweet_id: &'a str,
}

/// XPOST `meta.toml` (`kind = xpost`).
#[must_use]
pub fn pack_xpost_meta(input: &XpostMetaInput<'_>) -> String {
    let created = if input.created_at.is_empty() {
        Local::now().to_rfc3339()
    } else {
        input.created_at.to_string()
    };
    format!(
        "kind = \"xpost\"\n\
xpost_id = {xpost_id}\n\
tweet_id = {tweet_id}\n\
subject = {subject}\n\
created_at = \"{created}\"\n\
model = \"{model}\"\n\
tokens_in = {tin}\n\
tokens_out = {tout}\n\
cite = {cite}\n\
quote_tweet_id = {quote}\n\
sources = {sources}\n",
        xpost_id = toml_string(input.xpost_id),
        tweet_id = toml_string(input.tweet_id),
        subject = toml_string(input.subject),
        model = input.model.replace('"', ""),
        tin = input.tokens_in,
        tout = input.tokens_out,
        cite = toml_string(input.cite),
        quote = toml_string(input.quote_tweet_id),
        sources = sources_toml_list(input.sources),
    )
}

fn sources_toml_list(sources: &[String]) -> String {
    let mut out = String::from("[\n");
    for s in sources {
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = writeln!(out, "  \"{escaped}\",");
    }
    out.push(']');
    out
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Draft id from any segment in a draft `body.md` path (flat, legacy month, or day-sharded).
#[must_use]
pub fn draft_id_from_path(path: &str) -> Option<String> {
    let name = path.replace('\\', "/");
    let rest = name.strip_prefix("drafts/").unwrap_or(name.as_str());
    rest.split('/')
        .find(|seg| seg.starts_with("DRAFT-"))
        .map(std::string::ToString::to_string)
}

/// Alias kept for call sites that still say "drafts path".
#[must_use]
pub fn draft_id_from_drafts_path(path: &str) -> Option<String> {
    draft_id_from_path(path)
}

/// True when `path` is a Draft `body.md` (root id tree or legacy `drafts/` prefix).
#[must_use]
pub fn is_draft_body_path(path: &str) -> bool {
    let name = path.replace('\\', "/");
    name.ends_with("/body.md") && draft_id_from_path(&name).is_some()
}

/// Tweet id from `<TWEET-id>/…`.
#[must_use]
pub fn tweet_id_from_path(path: &str) -> Option<String> {
    let name = path.replace('\\', "/");
    name.split('/')
        .find(|seg| seg.starts_with("TWEET-"))
        .map(std::string::ToString::to_string)
}

/// True when `path` is a Tweet `body.md`.
#[must_use]
pub fn is_tweet_body_path(path: &str) -> bool {
    let name = path.replace('\\', "/");
    name.ends_with("/body.md") && tweet_id_from_path(&name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_to_post_id() {
        assert_eq!(
            draft_id_to_post_id("DRAFT-20260801-000025").as_deref(),
            Some("POST-20260801-000025")
        );
        assert!(draft_id_to_post_id("POST-20260801-000025").is_none());
    }

    #[test]
    fn pack_draft_paths() {
        let draft = PendingDraft {
            draft_id: "DRAFT-20260722-000001".into(),
            subject: "AI mascot intro".into(),
            body: "Hello world\n\nWritten by AI - ITCy - model ollama/llama3.2 - tokens in:1 out:2"
                .into(),
            model: "ollama/llama3.2".into(),
            tokens_in: 1,
            tokens_out: 2,
            sources: vec!["https://example.com/a".into()],
            link_options: Vec::new(),
            research_pack: String::new(),
            status: "open".into(),
            created_at: "2026-07-22T18:00:00Z".into(),
            updated_at: "2026-07-22T18:00:00Z".into(),
            fork_pr_number: None,
            fork_pr_url: String::new(),
        };
        let files = pack_draft_files(&draft);
        assert_eq!(files.draft_id, "DRAFT-20260722-000001");
        assert_eq!(files.body_path, "2026/07/22/DRAFT-20260722-000001/body.md");
        assert_eq!(
            files.meta_path,
            "2026/07/22/DRAFT-20260722-000001/meta.toml"
        );
        assert!(files.meta_toml.contains("kind = \"draft\""));
        assert!(files
            .meta_toml
            .contains("draft_id = \"DRAFT-20260722-000001\""));
        assert_eq!(
            branch_name_for_draft(&files.draft_id),
            "draft/DRAFT-20260722-000001"
        );
        assert_eq!(
            draft_id_from_path("DRAFT-20260722-000001/body.md").as_deref(),
            Some("DRAFT-20260722-000001")
        );
        assert_eq!(
            draft_id_from_path("drafts/DRAFT-20260722-000001/body.md").as_deref(),
            Some("DRAFT-20260722-000001")
        );
        assert_eq!(
            draft_id_from_path("2026/07/22/DRAFT-20260722-000001/body.md").as_deref(),
            Some("DRAFT-20260722-000001")
        );
        assert_eq!(
            post_paths("POST-20260722-000001"),
            (
                "2026/07/22/POST-20260722-000001/body.md".into(),
                "2026/07/22/POST-20260722-000001/meta.toml".into()
            )
        );
    }

    #[test]
    fn body_as_post_rewrites_header() {
        let body = "Draft ID: DRAFT-20260801-000025\n\nHello";
        let out = body_as_post(body, "POST-20260801-000025");
        assert!(out.starts_with("Post ID: POST-20260801-000025\n"));
        assert!(out.contains("Hello"));
        assert!(!out.contains("Draft ID:"));
    }

    #[test]
    fn tweet_to_xpost_and_pack() {
        assert_eq!(
            tweet_id_to_xpost_id("TWEET-20260813-000001").as_deref(),
            Some("XPOST-20260813-000001")
        );
        assert_eq!(
            xpost_id_to_tweet_id("XPOST-20260813-000001").as_deref(),
            Some("TWEET-20260813-000001")
        );
        assert_eq!(
            tweet_xpost_ids("TWEET-20260814-000010"),
            Some((
                "TWEET-20260814-000010".into(),
                "XPOST-20260814-000010".into()
            ))
        );
        assert_eq!(
            tweet_xpost_ids("XPOST-20260814-000010"),
            Some((
                "TWEET-20260814-000010".into(),
                "XPOST-20260814-000010".into()
            ))
        );
        assert_eq!(
            post_id_to_draft_id("POST-20260801-000025").as_deref(),
            Some("DRAFT-20260801-000025")
        );
        let draft = PendingDraft {
            draft_id: "TWEET-20260813-000001".into(),
            subject: "owl merge".into(),
            body: "Tweet ID: TWEET-20260813-000001\n\nBuilders, merge landed.".into(),
            model: "ollama/llama3.2".into(),
            tokens_in: 1,
            tokens_out: 2,
            sources: vec!["https://x.com/foo/status/99".into()],
            link_options: vec!["https://x.com/foo/status/99".into()],
            research_pack: String::new(),
            status: "open".into(),
            created_at: "2026-08-13T12:00:00Z".into(),
            updated_at: "2026-08-13T12:00:00Z".into(),
            fork_pr_number: None,
            fork_pr_url: String::new(),
        };
        let files = pack_tweet_files(&draft);
        assert_eq!(files.body_path, "2026/08/13/TWEET-20260813-000001/body.md");
        assert_eq!(
            files.meta_path,
            "2026/08/13/TWEET-20260813-000001/meta.toml"
        );
        assert!(files.meta_toml.contains("kind = \"tweet\""));
        assert!(files.meta_toml.contains("quote_tweet_id = \"99\""));
        assert_eq!(
            branch_name_for_tweet(&files.draft_id),
            "tweet/TWEET-20260813-000001"
        );
        let xbody = body_as_xpost(&draft.body, "XPOST-20260813-000001");
        assert!(xbody.starts_with("XPOST ID: XPOST-20260813-000001\n"));
        assert!(is_tweet_body_path("TWEET-20260813-000001/body.md"));
        assert!(is_tweet_body_path(
            "2026/08/13/TWEET-20260813-000001/body.md"
        ));
        assert!(is_tweet_body_path("2026/08/TWEET-20260813-000001/body.md"));
        assert_eq!(
            tweet_id_from_path("2026/08/13/TWEET-20260813-000001/meta.toml").as_deref(),
            Some("TWEET-20260813-000001")
        );
        assert_eq!(
            xpost_paths("XPOST-20260813-000001"),
            (
                "2026/08/13/XPOST-20260813-000001/body.md".into(),
                "2026/08/13/XPOST-20260813-000001/meta.toml".into()
            )
        );
        assert!(!is_tweet_body_path("DRAFT-20260813-000001/body.md"));
        assert_eq!(
            tweet_paths("TWEET-1"),
            ("TWEET-1/body.md".into(), "TWEET-1/meta.toml".into())
        );
    }
}

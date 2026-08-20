// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! linkedin-itcy-GR: stdio MCP for the operator personal `LinkedIn` corpus.
//!
//! Read-only tools over `sql/runtime.db` sources (export bootstrap + later Portability).
//! Not company CM (`linkedin-itcy-CM`).

use anyhow::Context;
use itcy::paths::product_join;
use itcy::sources::{SourceDb, SourceListFilter};
use rmcp::{
    handler::server::tool::ToolRouter,
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;

#[derive(Clone)]
struct LinkedinGrMcp {
    db_path: PathBuf,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LimitArgs {
    /// Max rows (default 20).
    #[serde(default = "default_limit_20")]
    limit: u32,
}

const fn default_limit_20() -> u32 {
    20
}

const fn default_limit_100() -> u32 {
    100
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CommentsArgs {
    #[serde(default = "default_limit_100")]
    limit: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchArgs {
    /// Substring match in title or body.
    query: String,
    /// Optional activity filter: post, comment, repost, profile, …
    #[serde(default)]
    activity: String,
    #[serde(default = "default_limit_20")]
    limit: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetArgs {
    id: i64,
}

#[tool_router]
impl LinkedinGrMcp {
    fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            tool_router: Self::tool_router(),
        }
    }

    fn open_db(&self) -> Result<SourceDb, McpError> {
        SourceDb::open(&self.db_path).map_err(|e| McpError::internal_error(e.to_string(), None))
    }

    fn list_activity(&self, activity: &str, limit: u32) -> Result<CallToolResult, McpError> {
        let db = self.open_db()?;
        let rows = db
            .list_sources(&SourceListFilter {
                activity: activity.into(),
                limit: limit.clamp(1, 500),
                preview_chars: 280,
                ..Default::default()
            })
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let payload = json!({
            "activity": activity,
            "count": rows.len(),
            "items": rows.iter().map(|r| json!({
                "id": r.id,
                "occurred_at": r.occurred_at,
                "title": r.title,
                "url": r.url,
                "preview": r.preview,
                "text_len": r.text_len,
                "subject": r.subject,
            })).collect::<Vec<_>>(),
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        )]))
    }

    #[tool(
        description = "Counts of operator personal LinkedIn corpus rows by kind and activity (post, comment, repost, profile, …). Reactions are out of scope."
    )]
    async fn linkedin_stats(&self) -> Result<CallToolResult, McpError> {
        let db = self.open_db()?;
        let counts = db
            .counts_by_activity()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let sources = db
            .source_count()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let chunks = db
            .chunk_count()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let payload = json!({
            "db": self.db_path.display().to_string(),
            "sources": sources,
            "chunks": chunks,
            "by_activity": counts.iter().map(|c| json!({
                "kind": c.kind,
                "activity": c.activity,
                "count": c.count,
            })).collect::<Vec<_>>(),
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        )]))
    }

    #[tool(
        description = "Operator personal LinkedIn profile / CV slice: headline, summary, positions, skills, education, certs"
    )]
    async fn linkedin_profile(&self) -> Result<CallToolResult, McpError> {
        let db = self.open_db()?;
        let mut items = Vec::new();
        for activity in [
            "profile",
            "position",
            "skill",
            "education",
            "certification",
            "honor",
            "language",
            "recommendation",
            "project",
        ] {
            let rows = db
                .list_sources(&SourceListFilter {
                    activity: activity.into(),
                    limit: 50,
                    preview_chars: 400,
                    ..Default::default()
                })
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            for r in rows {
                items.push(json!({
                    "id": r.id,
                    "activity": r.activity,
                    "title": r.title,
                    "preview": r.preview,
                    "url": r.url,
                }));
            }
        }
        let payload = json!({ "count": items.len(), "items": items });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        )]))
    }

    #[tool(
        description = "Last N operator personal LinkedIn posts (Shares), newest first by occurred_at"
    )]
    async fn linkedin_posts(
        &self,
        rmcp::handler::server::wrapper::Parameters(args): rmcp::handler::server::wrapper::Parameters<
            LimitArgs,
        >,
    ) -> Result<CallToolResult, McpError> {
        self.list_activity("post", args.limit)
    }

    #[tool(description = "Last N operator personal LinkedIn reposts, newest first")]
    async fn linkedin_reposts(
        &self,
        rmcp::handler::server::wrapper::Parameters(args): rmcp::handler::server::wrapper::Parameters<
            LimitArgs,
        >,
    ) -> Result<CallToolResult, McpError> {
        self.list_activity("repost", args.limit)
    }

    #[tool(description = "Last N operator personal LinkedIn comments, newest first")]
    async fn linkedin_comments(
        &self,
        rmcp::handler::server::wrapper::Parameters(args): rmcp::handler::server::wrapper::Parameters<
            CommentsArgs,
        >,
    ) -> Result<CallToolResult, McpError> {
        self.list_activity("comment", args.limit)
    }

    #[tool(
        description = "Reactions / likes are out of inspiration scope and are not imported. Returns an out-of-scope notice."
    )]
    async fn linkedin_reactions(
        &self,
        rmcp::handler::server::wrapper::Parameters(_args): rmcp::handler::server::wrapper::Parameters<
            LimitArgs,
        >,
    ) -> Result<CallToolResult, McpError> {
        let payload = json!({
            "out_of_scope": true,
            "message": "Reactions / likes are not imported and are not used for LinkedIn inspiration. Use posts, reposts, and comments.",
            "count": 0,
            "items": [],
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        )]))
    }

    #[tool(
        description = "Tor URL enrich queue status for link-only posts/reposts (pending / in_flight / ok / failed / next enrich_after)"
    )]
    async fn linkedin_enrich_status(&self) -> Result<CallToolResult, McpError> {
        let db = self.open_db()?;
        let counts = db
            .enrich_status_counts()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let remaining = counts.pending + counts.failed + counts.in_flight;
        let payload = json!({
            "db": self.db_path.display().to_string(),
            "queue_remaining_estimate": remaining,
            "pending": counts.pending,
            "in_flight": counts.in_flight,
            "ok": counts.ok,
            "failed": counts.failed,
            "skip": counts.skip,
            "none": counts.none,
            "next_enrich_after": counts.next_enrich_after,
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        )]))
    }

    #[tool(description = "Search operator personal LinkedIn corpus by text (title + body)")]
    async fn linkedin_search(
        &self,
        rmcp::handler::server::wrapper::Parameters(args): rmcp::handler::server::wrapper::Parameters<
            SearchArgs,
        >,
    ) -> Result<CallToolResult, McpError> {
        let db = self.open_db()?;
        let rows = db
            .search_sources(&args.query, &args.activity, 280, args.limit.clamp(1, 200))
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let payload = json!({
            "query": args.query,
            "activity": args.activity,
            "count": rows.len(),
            "items": rows.iter().map(|r| json!({
                "id": r.id,
                "activity": r.activity,
                "occurred_at": r.occurred_at,
                "title": r.title,
                "url": r.url,
                "preview": r.preview,
            })).collect::<Vec<_>>(),
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        )]))
    }

    #[tool(description = "Full source row by id (includes raw_text)")]
    async fn linkedin_get_source(
        &self,
        rmcp::handler::server::wrapper::Parameters(args): rmcp::handler::server::wrapper::Parameters<
            GetArgs,
        >,
    ) -> Result<CallToolResult, McpError> {
        let db = self.open_db()?;
        let Some(row) = db
            .get_source(args.id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
        else {
            return Err(McpError::invalid_params(
                format!("no source id {}", args.id),
                None,
            ));
        };
        let payload = json!({
            "id": row.id,
            "kind": row.kind,
            "activity": row.activity,
            "subject": row.subject,
            "title": row.title,
            "url": row.url,
            "occurred_at": row.occurred_at,
            "raw_text": row.raw_text,
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        )]))
    }
}

// rmcp `tool_handler` expands ServerHandler methods as `async` with no `.await`.
#[allow(clippy::unused_async_trait_impl)]
#[tool_handler(router = self.tool_router)]
impl ServerHandler for LinkedinGrMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "linkedin-itcy-GR: operator personal LinkedIn corpus (export + Portability). Use linkedin_stats, linkedin_posts, linkedin_comments, linkedin_reposts, linkedin_profile, linkedin_search, linkedin_enrich_status. Reactions are out of scope. Not company page CM.",
        )
    }
}

fn resolve_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("ITCY_STATE_DB") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(p) = std::env::var("ITCY_CORPUS_DB") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    product_join("sql/runtime.db")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    itcy::logging::init_tracing_stderr("warn");
    let db = resolve_db_path();
    tracing::info!(path = %db.display(), "linkedin-itcy-GR opening corpus db");
    let service = LinkedinGrMcp::new(db)
        .serve(rmcp::transport::stdio())
        .await
        .context("start linkedin-itcy-GR MCP stdio server")?;
    service.waiting().await.context("MCP server wait")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use itcy::sources::InsertSource;
    use tempfile::TempDir;

    fn seed_db(path: &std::path::Path) {
        let db = SourceDb::open(path).expect("open");
        db.insert_source(&InsertSource {
            kind: "personal_feed",
            activity: "post",
            subject: "rust",
            title: "Old post",
            url: Some("https://li/old"),
            raw_text: "old rust note",
            occurred_at: Some("2020-01-01T00:00:00"),
        })
        .expect("old");
        db.insert_source(&InsertSource {
            kind: "personal_feed",
            activity: "post",
            subject: "rust",
            title: "New post",
            url: Some("https://li/new"),
            raw_text: "new rust note",
            occurred_at: Some("2025-06-01T12:00:00"),
        })
        .expect("new");
        db.insert_source(&InsertSource {
            kind: "comment",
            activity: "comment",
            subject: "tdd",
            title: "Agree",
            url: Some("https://li/c1"),
            raw_text: "Agree - TDD first",
            occurred_at: Some("2025-05-01T10:00:00"),
        })
        .expect("comment");
        db.insert_source(&InsertSource {
            kind: "personal_feed",
            activity: "repost",
            subject: "share",
            title: "Repost",
            url: Some("https://li/rp1"),
            raw_text: "Repost\nhttps://li/rp1",
            occurred_at: Some("2025-07-01T08:00:00"),
        })
        .expect("repost");
        db.insert_source(&InsertSource {
            kind: "voice",
            activity: "profile",
            subject: "profile",
            title: "Headline - Greg Test",
            url: None,
            raw_text: "Senior Rust Engineer",
            occurred_at: None,
        })
        .expect("profile");
    }

    #[tokio::test]
    async fn tools_return_ordered_posts_and_stats() {
        let dir = TempDir::new().expect("temp");
        let db_path = dir.path().join("t.db");
        seed_db(&db_path);
        let mcp = LinkedinGrMcp::new(db_path);
        let posts = mcp.list_activity("post", 10).expect("posts");
        let text = posts.content[0].as_text().expect("text").text.clone();
        assert!(text.contains("New post"));
        let new_pos = text.find("New post").expect("new");
        let old_pos = text.find("Old post").expect("old");
        assert!(new_pos < old_pos, "newest first");
        let stats = mcp.linkedin_stats().await.expect("stats");
        let stats_text = stats.content[0].as_text().expect("text").text.clone();
        assert!(stats_text.contains("post"));
        assert!(stats_text.contains("repost"));
        let reactions = mcp
            .linkedin_reactions(rmcp::handler::server::wrapper::Parameters(LimitArgs {
                limit: 5,
            }))
            .await
            .expect("reactions");
        let rtext = reactions.content[0].as_text().expect("text").text.clone();
        assert!(rtext.contains("out_of_scope"));
        let enrich = mcp.linkedin_enrich_status().await.expect("enrich");
        let etext = enrich.content[0].as_text().expect("text").text.clone();
        assert!(etext.contains("pending") || etext.contains("ok") || etext.contains("none"));
    }
}

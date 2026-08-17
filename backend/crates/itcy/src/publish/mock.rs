// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Playground company-page publisher (no `LinkedIn` HTTP).

use super::{PublishError, PublishMode, PublishRequest, PublishResult, Publisher};
use async_trait::async_trait;
use tracing::info;

/// In-process publisher that records posts without `LinkedIn` HTTP.
pub struct PlaygroundPublisher;

#[async_trait]
impl Publisher for PlaygroundPublisher {
    fn mode(&self) -> PublishMode {
        PublishMode::Playground
    }

    async fn publish_company_post(
        &self,
        request: &PublishRequest,
    ) -> Result<PublishResult, PublishError> {
        let slug = request
            .draft_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .map_or_else(
                || {
                    request
                        .pubs_pr_number
                        .map_or_else(|| "unknown".into(), |n| format!("pr-{n}"))
                },
                |s| s.replace(['/', ' '], "-"),
            );
        let urn = format!("urn:li:share:playground-{slug}");
        let url = format!("https://www.linkedin.com/feed/update/{urn}");
        let detail = match (request.draft_id.as_deref(), request.pubs_pr_number) {
            (Some(d), Some(pr)) => {
                format!("playground ship ok draft={d} pubs_pr=#{pr} urn={urn}")
            }
            (Some(d), None) => format!("playground ship ok draft={d} urn={urn}"),
            (None, Some(pr)) => format!("playground ship ok pubs_pr=#{pr} urn={urn}"),
            (None, None) => format!("playground ship ok urn={urn}"),
        };
        info!(
            draft_id = request.draft_id.as_deref().unwrap_or(""),
            pubs_pr = request.pubs_pr_number.unwrap_or(0),
            body_chars = request.body.chars().count(),
            %urn,
            "publish: playground company post (no LinkedIn HTTP)"
        );
        Ok(PublishResult {
            mode: PublishMode::Playground,
            linkedin_urn: Some(urn),
            linkedin_url: Some(url),
            detail,
        })
    }
}

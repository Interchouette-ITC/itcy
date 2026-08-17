// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Production Community Management publisher (company page posts).
//!
//! Refuses to construct without `LINKEDIN_ACCESS_TOKEN` and a non-empty org id.
//! Without CM approval the token will not have page scopes; that surfaces as HTTP
//! errors at publish time, not as a silent playground.

use super::{
    resolve_linkedin_organization_id, PublishError, PublishMode, PublishRequest, PublishResult,
    Publisher, CM_API_VERSION,
};
use crate::sources::resolve_linkedin_access_token;
use crate::sources::url_hygiene::LINKEDIN_REST_POSTS_URL;
use async_trait::async_trait;
use tracing::{info, warn};

/// Real `LinkedIn` REST posts client for the company page.
pub struct ProductionLinkedInPublisher {
    token: String,
    organization_id: String,
    http: reqwest::Client,
}

impl ProductionLinkedInPublisher {
    /// Builds from env / `.linkedin`. Errors if token or org id missing.
    ///
    /// # Errors
    ///
    /// Returns a [`PublishError`] variant for mode, token, or `LinkedIn` publish failure.
    pub fn try_new() -> Result<Self, PublishError> {
        let token = resolve_linkedin_access_token();
        let org = resolve_linkedin_organization_id();
        Self::try_from_parts(token, Some(org))
    }

    /// Test / explicit constructor. Empty org string is treated as missing.
    ///
    /// # Errors
    ///
    /// Returns a [`PublishError`] variant for mode, token, or `LinkedIn` publish failure.
    pub fn try_from_parts(
        token: Option<String>,
        organization_id: Option<String>,
    ) -> Result<Self, PublishError> {
        let token = token
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                PublishError::Credentials(
                    "LINKEDIN_ACCESS_TOKEN missing (CM Development approval + paste into .linkedin)"
                        .into(),
                )
            })?;
        let organization_id = organization_id
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                PublishError::Credentials(
                    "LINKEDIN_ORGANIZATION_ID missing (numeric company page id)".into(),
                )
            })?;
        Ok(Self {
            token,
            organization_id,
            http: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl Publisher for ProductionLinkedInPublisher {
    fn mode(&self) -> PublishMode {
        PublishMode::Production
    }

    async fn publish_company_post(
        &self,
        request: &PublishRequest,
    ) -> Result<PublishResult, PublishError> {
        let body = request.body.trim();
        if body.is_empty() {
            return Err(PublishError::Other("empty post body".into()));
        }

        let author = format!("urn:li:organization:{}", self.organization_id);
        let payload = serde_json::json!({
            "author": author,
            "commentary": body,
            "visibility": "PUBLIC",
            "distribution": {
                "feedDistribution": "MAIN_FEED",
                "targetEntities": [],
                "thirdPartyDistributionChannels": []
            },
            "lifecycleState": "PUBLISHED",
            "isReshareDisabledByAuthor": false
        });

        info!(
            org = %self.organization_id,
            draft_id = request.draft_id.as_deref().unwrap_or(""),
            pubs_pr = request.pubs_pr_number.unwrap_or(0),
            body_chars = body.chars().count(),
            "publish: live company post request"
        );

        let res = self
            .http
            .post(LINKEDIN_REST_POSTS_URL)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Linkedin-Version", CM_API_VERSION)
            .header("X-Restli-Protocol-Version", "2.0.0")
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| PublishError::Http(e.to_string()))?;

        let status = res.status();
        let urn_header = res
            .headers()
            .get("x-restli-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let text = res
            .text()
            .await
            .map_err(|e| PublishError::Http(e.to_string()))?;

        if !status.is_success() {
            warn!(
                %status,
                body = %truncate_for_log(&text, 400),
                "publish: live LinkedIn post failed"
            );
            return Err(PublishError::Http(format!(
                "{status}: {}",
                truncate_for_log(&text, 800)
            )));
        }

        let urn = urn_header.or_else(|| extract_urn_from_body(&text));
        let url = urn
            .as_ref()
            .map(|u| format!("https://www.linkedin.com/feed/update/{u}"));
        let detail = match (&urn, request.draft_id.as_deref(), request.pubs_pr_number) {
            (Some(u), Some(d), Some(pr)) => {
                format!("live ship ok draft={d} pubs_pr=#{pr} urn={u}")
            }
            (Some(u), Some(d), None) => format!("live ship ok draft={d} urn={u}"),
            (Some(u), None, Some(pr)) => format!("live ship ok pubs_pr=#{pr} urn={u}"),
            (Some(u), None, None) => format!("live ship ok urn={u}"),
            (None, _, _) => "live ship ok (no URN in response)".into(),
        };

        info!(
            urn = urn.as_deref().unwrap_or(""),
            "publish: live company post ok"
        );

        Ok(PublishResult {
            mode: PublishMode::Production,
            linkedin_urn: urn,
            linkedin_url: url,
            detail,
        })
    }
}

fn truncate_for_log(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn extract_urn_from_body(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("id")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .or_else(|| {
            v.get("entity")
                .and_then(|x| x.get("id"))
                .and_then(|x| x.as_str())
                .map(str::to_string)
        })
}

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! INTERCHOUETTE digest lane: catalog + public GitHub activity (5 draft + 5 tweet).

use crate::bat::github::github_token_from_env_or_creds;
use crate::sources::itc_catalog::{
    digest_eligible, github_probe_target, load_itc_catalog, public_cite_for, site_cv, site_home,
    site_news, ItcCatalogEntry,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use tracing::{info, warn};

const MAX_ITC_ITEMS: usize = 10;
const MAX_ITC_DRAFT: usize = 5;
const MAX_ITC_TWEET: usize = 5;

/// Candidate shaped for [`crate::sources::digest`] lane mix.
#[derive(Debug, Clone)]
pub struct ItcDigestCandidate {
    pub title: String,
    pub url: Option<String>,
    pub subject: String,
    pub lane: String,
    pub weight: i32,
    pub detail: String,
}

#[derive(Debug, Deserialize)]
struct GhRepo {
    private: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    pushed_at: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
}

#[derive(Debug, Clone)]
struct RankedProject {
    branch: String,
    title: String,
    cite_url: String,
    blurb: String,
    pushed_at: String,
    weight: i32,
}

/// Collects up to 10 INTERCHOUETTE items (5 `itc_draft` + 5 `itc_tweet`).
pub async fn collect_itc_candidates() -> Vec<ItcDigestCandidate> {
    let entries = match load_itc_catalog() {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "digest: itc catalog unavailable");
            return Vec::new();
        }
    };
    let eligible = digest_eligible(&entries);
    if eligible.is_empty() {
        return Vec::new();
    }
    let token = github_token_from_env_or_creds();
    let prefer_news = news_is_live().await;
    let mut ranked = Vec::new();
    for entry in eligible {
        if let Some(r) = rank_one(entry, token.as_deref(), prefer_news).await {
            ranked.push(r);
        } else {
            warn!(branch = %entry.branch, "digest: itc project skipped (no public cite)");
        }
    }
    ranked.sort_by(|a, b| {
        b.pushed_at
            .cmp(&a.pushed_at)
            .then_with(|| b.weight.cmp(&a.weight))
            .then_with(|| a.branch.cmp(&b.branch))
    });
    ranked.truncate(MAX_ITC_ITEMS);
    let out = label_five_five(ranked);
    info!(n = out.len(), "digest: itc candidates ready");
    out
}

async fn news_is_live() -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
    else {
        return false;
    };
    client
        .head(site_news())
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

async fn rank_one(
    entry: &ItcCatalogEntry,
    token: Option<&str>,
    prefer_news: bool,
) -> Option<RankedProject> {
    let cite = public_cite_for(&entry.branch);
    let mut cite_url = cite.html_url.clone();
    let mut blurb = format!("Interchouette project `{}`.", entry.branch);
    let mut pushed_at = String::new();
    let mut weight = 6;

    if let Some((owner, repo)) = github_probe_target(&entry.branch) {
        if let Some(gh) = probe_repo(&owner, &repo, token).await {
            if gh.private {
                // Keep non-GitHub public cites (site / worker profile); never emit private URL.
                if cite_url.contains("github.com/Interchouette-ITC/")
                    && entry.branch != "itcy"
                    && entry.branch != "interchouette"
                {
                    cite_url = if prefer_news {
                        site_news().into()
                    } else {
                        site_home().into()
                    };
                }
            } else if let Some(html) = gh.html_url.filter(|u| !u.is_empty()) {
                // Prefer catalog public cite for itcy / site; else GitHub html_url.
                if entry.branch != "itcy" && entry.branch != "interchouette" {
                    cite_url = html;
                }
            }
            if let Some(d) = gh.description.filter(|s| !s.trim().is_empty()) {
                blurb = d.trim().to_string();
            }
            if let Some(p) = gh.pushed_at {
                pushed_at = p;
                weight = 9;
            }
        }
    }

    if entry.branch == "interchouette" {
        cite_url = if prefer_news {
            site_news().into()
        } else {
            site_home().into()
        };
        blurb = format!("{blurb} Site: {cite_url}");
    }
    if entry.branch == "itcy" && blurb.len() < 40 {
        blurb = "ITCy: Interchouette AI mascot and LinkedIn/X operator experiment.".into();
    }

    Some(RankedProject {
        branch: entry.branch.clone(),
        title: entry.branch.clone(),
        cite_url,
        blurb,
        pushed_at,
        weight,
    })
}

async fn probe_repo(owner: &str, repo: &str, token: Option<&str>) -> Option<GhRepo> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .ok()?;
    let url = format!("https://api.github.com/repos/{owner}/{repo}");
    let mut req = client
        .get(&url)
        .header(USER_AGENT, "itcy-digest")
        .header(ACCEPT, "application/vnd.github+json");
    if let Some(t) = token {
        req = req.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<GhRepo>().await.ok()
}

fn label_five_five(ranked: Vec<RankedProject>) -> Vec<ItcDigestCandidate> {
    let mut drafts = 0usize;
    let mut tweets = 0usize;
    let mut out = Vec::with_capacity(ranked.len());
    for (i, r) in ranked.into_iter().enumerate() {
        let want_draft = i % 2 == 0;
        let lane = if want_draft && drafts < MAX_ITC_DRAFT {
            drafts += 1;
            "itc_draft"
        } else if tweets < MAX_ITC_TWEET {
            tweets += 1;
            "itc_tweet"
        } else if drafts < MAX_ITC_DRAFT {
            drafts += 1;
            "itc_draft"
        } else {
            break;
        };
        let label = if lane == "itc_draft" {
            "DRAFT"
        } else {
            "TWEET"
        };
        out.push(ItcDigestCandidate {
            title: format!("{label} · {}", r.title),
            url: Some(r.cite_url),
            subject: format!("Interchouette project {}", r.branch),
            lane: lane.into(),
            weight: r.weight,
            detail: r.blurb,
        });
    }
    out
}

/// Builds an ITC `ResearchPack` for slash `/draft_about_itc` / `/draft_tweet_about_itc`.
///
/// # Errors
///
/// Returns an error when the catalog is missing or no public cite can be built.
pub async fn build_itc_research_pack(subject: &str) -> Result<(String, Vec<String>), String> {
    let entries = load_itc_catalog()?;
    let picked = pick_entry_for_subject(&entries, subject);
    let prefer_news = news_is_live().await;
    let token = github_token_from_env_or_creds();
    let ranked = match &picked {
        Some(e) => rank_one(e, token.as_deref(), prefer_news)
            .await
            .ok_or_else(|| format!("no public cite for `{}`", e.branch))?,
        None => RankedProject {
            branch: "interchouette".into(),
            title: "Interchouette".into(),
            cite_url: if prefer_news {
                site_news().into()
            } else {
                site_home().into()
            },
            blurb: "Interchouette ITC brand and projects.".into(),
            pushed_at: String::new(),
            weight: 5,
        },
    };

    let profile_org = fetch_text(
        "https://raw.githubusercontent.com/Interchouette-ITC/.github/dev/profile/README.md",
    )
    .await
    .unwrap_or_default();
    let profile_worker =
        fetch_text("https://raw.githubusercontent.com/Interchouette/Interchouette/dev/README.md")
            .await
            .unwrap_or_default();
    let cv_html = fetch_text(site_cv()).await.unwrap_or_default();
    let cv_excerpt = plain_excerpt(&cv_html, 1200);
    let org_excerpt = plain_excerpt(&profile_org, 800);
    let worker_excerpt = plain_excerpt(&profile_worker, 800);

    let mut urls = vec![ranked.cite_url.clone()];
    if prefer_news {
        urls.push(site_news().into());
    } else {
        urls.push(site_home().into());
    }
    urls.push(site_cv().into());
    urls.dedup();

    let pack = format!(
        "## ResearchPack\n\
subject: {subject}\n\
summary: Interchouette / ITC portfolio grounding. Voice = ITCy (AI mascot), never Greg first-person.\n\
project: {branch}\n\
blurb: {blurb}\n\
candidates:\n\
- final_url={cite} | title={title} | why=primary public cite\n\
- final_url={site} | title=interchouette.net | why=brand site\n\
- final_url={cv_url} | title=founder CV | why=company/founder context only\n\
org_profile:\n{org}\n\
worker_profile:\n{worker}\n\
cv_excerpt:\n{cv_text}\n\
notes: Cite ONLY URLs listed in candidates. No press hubs. No LinkedIn. No private GitHub repos.\n",
        subject = subject.trim(),
        branch = ranked.branch,
        blurb = ranked.blurb,
        cite = ranked.cite_url,
        title = ranked.title,
        site = if prefer_news { site_news() } else { site_home() },
        cv_url = site_cv(),
        org = org_excerpt,
        worker = worker_excerpt,
        cv_text = cv_excerpt,
    );
    Ok((pack, urls))
}

fn pick_entry_for_subject<'a>(
    entries: &'a [ItcCatalogEntry],
    subject: &str,
) -> Option<&'a ItcCatalogEntry> {
    let s = subject.trim().to_ascii_lowercase();
    let eligible = digest_eligible(entries);
    if s.is_empty() || s == "what we know" {
        return eligible.into_iter().next();
    }
    eligible
        .iter()
        .copied()
        .find(|e| {
            let b = e.branch.to_ascii_lowercase();
            s.contains(&b) || b.contains(&s) || e.product_root.to_ascii_lowercase().contains(&s)
        })
        .or_else(|| digest_eligible(entries).into_iter().next())
}

/// Top-ranked eligible catalog subject when slash args are empty.
///
/// # Errors
///
/// Returns an error when the catalog is missing or no project ranks.
pub async fn default_itc_subject() -> Result<String, String> {
    let entries = load_itc_catalog()?;
    let eligible = digest_eligible(&entries);
    let token = github_token_from_env_or_creds();
    let prefer_news = news_is_live().await;
    let mut ranked = Vec::new();
    for e in eligible {
        if let Some(r) = rank_one(e, token.as_deref(), prefer_news).await {
            ranked.push(r);
        }
    }
    ranked.sort_by(|a, b| b.pushed_at.cmp(&a.pushed_at));
    ranked
        .into_iter()
        .next()
        .map(|r| format!("Interchouette project {}", r.branch))
        .ok_or_else(|| "no Interchouette projects available in catalog".into())
}

async fn fetch_text(url: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .ok()?;
    let resp = client
        .get(url)
        .header(USER_AGENT, "itcy-itc-pack")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().await.ok()
}

fn plain_excerpt(raw: &str, max: usize) -> String {
    let plain = crate::sources::html::html_to_text(raw);
    let t = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= max {
        return t;
    }
    t.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_five_five_alternates() {
        let ranked: Vec<_> = (0..10)
            .map(|i| RankedProject {
                branch: format!("p{i}"),
                title: format!("p{i}"),
                cite_url: format!("https://example.com/{i}"),
                blurb: "blurb".into(),
                pushed_at: format!("2026-08-{i:02}T00:00:00Z"),
                weight: 8,
            })
            .collect();
        let out = label_five_five(ranked);
        assert_eq!(out.len(), 10);
        assert_eq!(out.iter().filter(|c| c.lane == "itc_draft").count(), 5);
        assert_eq!(out.iter().filter(|c| c.lane == "itc_tweet").count(), 5);
        assert!(out[0].title.starts_with("DRAFT"));
        assert!(out[1].title.starts_with("TWEET"));
    }
}

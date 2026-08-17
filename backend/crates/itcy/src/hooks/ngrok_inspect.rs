// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Poll ngrok local inspect API for tunnel-side webhook fails.
//!
//! Catches 502 / wrong-path deliveries that never reach the `ITCy` handler.
//! Same signal as polling the local ngrok inspect API, recorded on `GithubHookState` for `/status`.

use crate::hooks::GithubHookState;
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;
use tracing::debug;

const DEFAULT_API: &str = "http://127.0.0.1:4040/api/requests/http";
const POLL_SECS: u64 = 2;
const LIMIT: u32 = 20;

const OK_PATHS: &[&str] = &[
    "/github/webhook_ITC",
    "/github/webhook_ITCy",
    "/github/webhook_ICA",
];

/// Background loop: poll ngrok inspect and set `delivery_warn` on tunnel-side fails.
pub async fn run_ngrok_inspect_loop(hooks: GithubHookState) {
    let api = std::env::var("NGROK_INSPECT_API")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_API.to_string());
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            debug!(error = %e, "ngrok_inspect: failed to build HTTP client");
            return;
        }
    };
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if let Ok(new_ids) = poll_once(&client, &api, &seen, &hooks).await {
            for id in new_ids {
                seen.insert(id);
            }
            if seen.len() > 500 {
                seen.clear();
            }
        } else {
            // Unreachable already recorded on hooks.
        }
        tokio::time::sleep(Duration::from_secs(POLL_SECS)).await;
    }
}

const DEFAULT_TUNNELS_API: &str = "http://127.0.0.1:4040/api/tunnels";

async fn poll_once(
    client: &reqwest::Client,
    api: &str,
    seen: &HashSet<String>,
    hooks: &GithubHookState,
) -> Result<Vec<String>, ()> {
    if !tunnels_online(client).await {
        hooks.set_delivery_warn("ngrok tunnel offline (no public URL)");
        return Err(());
    }

    let url = format!("{api}?limit={LIMIT}");
    let resp = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => {
            hooks.set_delivery_warn("ngrok inspect unreachable");
            return Err(());
        }
    };
    let data: InspectResponse = if let Ok(d) = resp.json().await {
        d
    } else {
        hooks.set_delivery_warn("ngrok inspect bad json");
        return Err(());
    };

    let mut new_ids = Vec::new();
    for r in data.requests.into_iter().rev() {
        let rid = match r.id {
            Some(ref id) if !id.is_empty() && !seen.contains(id) => id.clone(),
            _ => continue,
        };
        new_ids.push(rid.clone());

        let method = r
            .request
            .as_ref()
            .and_then(|q| q.method.as_deref())
            .unwrap_or("");
        let uri = r
            .request
            .as_ref()
            .and_then(|q| q.uri.as_deref())
            .unwrap_or("");
        let status_raw = r
            .response
            .as_ref()
            .and_then(|s| s.status.as_deref())
            .unwrap_or("?");
        let code = status_raw.split_whitespace().next().unwrap_or(status_raw);
        let event = header_first(r.request.as_ref(), "X-Github-Event");

        let mut flags = Vec::new();
        if method == "POST" && !OK_PATHS.contains(&uri) {
            flags.push("WRONG_PATH");
        }
        if matches!(code, "404" | "405" | "502" | "503") {
            flags.push("BAD_STATUS");
        }
        if code == "401" && !event.is_empty() {
            flags.push("HMAC?");
        }
        if flags.is_empty() {
            continue;
        }
        let msg = format!(
            "{} {} {} -> {} event={}",
            flags.join(","),
            method,
            uri,
            status_raw,
            if event.is_empty() { "-" } else { &event }
        );
        hooks.set_delivery_warn(msg);
    }
    Ok(new_ids)
}

async fn tunnels_online(client: &reqwest::Client) -> bool {
    let tunnels_api = std::env::var("NGROK_TUNNELS_API")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_TUNNELS_API.to_string());
    let resp = match client.get(&tunnels_api).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return false,
    };
    let data: TunnelsResponse = match resp.json().await {
        Ok(d) => d,
        Err(_) => return false,
    };
    data.tunnels.iter().any(|t| {
        t.public_url
            .as_deref()
            .is_some_and(|u| !u.trim().is_empty())
    })
}

fn header_first(req: Option<&InspectRequest>, name: &str) -> String {
    let Some(req) = req else {
        return String::new();
    };
    let Some(headers) = req.headers.as_ref() else {
        return String::new();
    };
    for (k, v) in headers {
        if k.eq_ignore_ascii_case(name) {
            if let Some(first) = v.first() {
                return first.clone();
            }
        }
    }
    String::new()
}

#[derive(Debug, Deserialize)]
struct TunnelsResponse {
    #[serde(default)]
    tunnels: Vec<TunnelEntry>,
}

#[derive(Debug, Deserialize)]
struct TunnelEntry {
    public_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InspectResponse {
    #[serde(default)]
    requests: Vec<InspectEntry>,
}

#[derive(Debug, Deserialize)]
struct InspectEntry {
    id: Option<String>,
    request: Option<InspectRequest>,
    response: Option<InspectResponseBody>,
}

#[derive(Debug, Deserialize)]
struct InspectRequest {
    method: Option<String>,
    uri: Option<String>,
    headers: Option<std::collections::HashMap<String, Vec<String>>>,
}

#[derive(Debug, Deserialize)]
struct InspectResponseBody {
    status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_paths_include_org_ingress_and_legacy() {
        assert!(OK_PATHS.contains(&"/github/webhook_ITC"));
        assert!(OK_PATHS.contains(&"/github/webhook_ITCy"));
        assert!(OK_PATHS.contains(&"/github/webhook_ICA"));
    }
}

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! `LinkedIn` HTTP MCP listen probe (boot, `/status`, background watch).
//!
//! Same idea as Tor listen: surface when production ship dependency is down.
//! Does **not** refuse boot. Auth lives inside the MCP process (`LINKEDIN_ACCESS_TOKEN`);
//! this probe only checks the loopback HTTP port is accepting TCP.

use super::mcp::resolve_mcp_url;
use serde::Serialize;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;
use tracing::{info, warn};

const PROBE_TIMEOUT: Duration = Duration::from_millis(400);
const WATCH_SECS: u64 = 15;

/// Listen snapshot for operators / TUI / `/status`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LinkedInMcpStatus {
    pub ok: bool,
    pub url: String,
    pub listen_addr: String,
    /// Human line: `ok` or what is missing.
    pub detail: String,
}

/// TCP connect probe against the host:port from the MCP URL.
#[must_use]
pub fn probe_linkedin_mcp() -> LinkedInMcpStatus {
    let url = resolve_mcp_url();
    let listen_addr = host_port_from_url(&url).unwrap_or_else(|| "127.0.0.1:4780".into());
    let ok = tcp_open(&listen_addr);
    let detail = if ok {
        "ok".into()
    } else {
        format!("{listen_addr} down (stack-up / linkedin-vahabcore-up; production ship needs MCP)")
    };
    LinkedInMcpStatus {
        ok,
        url,
        listen_addr,
        detail,
    }
}

/// Logs current `LinkedIn` MCP listen status once.
pub fn log_linkedin_mcp_status(st: &LinkedInMcpStatus) {
    if st.ok {
        info!(url = %st.url, addr = %st.listen_addr, "linkedin-mcp: listening");
    } else {
        warn!(
            url = %st.url,
            addr = %st.listen_addr,
            detail = %st.detail,
            "linkedin-mcp: down"
        );
    }
}

/// Background watch: log when `LinkedIn` MCP listen state flips.
pub async fn run_linkedin_mcp_watch_loop() {
    let mut last_ok = Some(probe_linkedin_mcp().ok);
    loop {
        tokio::time::sleep(Duration::from_secs(WATCH_SECS)).await;
        let st = probe_linkedin_mcp();
        if last_ok != Some(st.ok) {
            log_linkedin_mcp_status(&st);
        }
        last_ok = Some(st.ok);
    }
}

fn host_port_from_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let hostport = rest.split('/').next().unwrap_or(rest).trim();
    if hostport.is_empty() {
        None
    } else {
        Some(hostport.to_string())
    }
}

fn tcp_open(addr: &str) -> bool {
    let Ok(mut addrs) = addr.to_socket_addrs() else {
        return false;
    };
    let Some(sa) = addrs.next() else {
        return false;
    };
    TcpStream::connect_timeout(&sa, PROBE_TIMEOUT).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_strips_path() {
        assert_eq!(
            host_port_from_url("http://127.0.0.1:4780/mcp"),
            Some("127.0.0.1:4780".into())
        );
    }

    #[test]
    fn probe_returns_detail_when_down() {
        std::env::set_var("ITCY_LINKEDIN_MCP_URL", "http://127.0.0.1:1/mcp");
        let st = probe_linkedin_mcp();
        assert!(!st.ok);
        assert!(st.detail.contains("down"));
        std::env::remove_var("ITCY_LINKEDIN_MCP_URL");
    }
}

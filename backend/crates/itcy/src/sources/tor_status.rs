// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Tor SOCKS + control listen probe (boot, `/status`, background watch).
//!
//! Same idea as ngrok inspect: surface when a stack dependency for `/enrich` is down.
//! Does **not** refuse boot (unlike Ollama warm).

use crate::sources::enrich::{DEFAULT_TOR_CONTROL, DEFAULT_TOR_SOCKS};
use serde::Serialize;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;
use tracing::{info, warn};

const PROBE_TIMEOUT: Duration = Duration::from_millis(400);
const WATCH_SECS: u64 = 15;

/// Listen snapshot for operators / TUI / `/status`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TorListenStatus {
    pub ok: bool,
    pub socks_ok: bool,
    pub control_ok: bool,
    pub socks_addr: String,
    pub control_addr: String,
    /// Human line: `ok` or what is missing.
    pub detail: String,
}

/// Resolves SOCKS URL / host:port env the same way `/enrich` does.
#[must_use]
pub fn resolve_tor_socks_probe_addr() -> String {
    let raw = std::env::var("ITCY_TOR_SOCKS").unwrap_or_else(|_| DEFAULT_TOR_SOCKS.to_string());
    socks_url_to_host_port(&raw).unwrap_or_else(|| "127.0.0.1:9050".into())
}

/// Resolves control `host:port` for NEWNYM.
#[must_use]
pub fn resolve_tor_control_addr() -> String {
    std::env::var("ITCY_TOR_CONTROL").unwrap_or_else(|_| DEFAULT_TOR_CONTROL.to_string())
}

/// TCP connect probe for SOCKS + control (no Tor protocol handshake).
#[must_use]
pub fn probe_tor_listen() -> TorListenStatus {
    let socks_addr = resolve_tor_socks_probe_addr();
    let control_addr = resolve_tor_control_addr();
    let socks_ok = tcp_open(&socks_addr);
    let control_ok = tcp_open(&control_addr);
    let ok = socks_ok && control_ok;
    let detail = if ok {
        "ok".into()
    } else {
        let mut missing = Vec::new();
        if !socks_ok {
            missing.push(format!("SOCKS {socks_addr} down"));
        }
        if !control_ok {
            missing.push(format!("control {control_addr} down"));
        }
        format!("{} (make tor-up; /enrich needs Tor)", missing.join("; "))
    };
    TorListenStatus {
        ok,
        socks_ok,
        control_ok,
        socks_addr,
        control_addr,
        detail,
    }
}

/// Logs current Tor listen status once (`info` when up, `warn` when down).
pub fn log_tor_listen_status(st: &TorListenStatus) {
    if st.ok {
        info!(
            socks = %st.socks_addr,
            control = %st.control_addr,
            "tor: SOCKS + control listening"
        );
    } else {
        warn!(
            socks = %st.socks_addr,
            socks_ok = st.socks_ok,
            control = %st.control_addr,
            control_ok = st.control_ok,
            detail = %st.detail,
            "tor: down"
        );
    }
}

/// Background watch: log when Tor listen state flips (ngrok-inspect style).
/// Boot already logged once; this loop seeds silently then warns/infos on change.
pub async fn run_tor_listen_watch_loop() {
    let mut last_ok = Some(probe_tor_listen().ok);
    loop {
        tokio::time::sleep(Duration::from_secs(WATCH_SECS)).await;
        let st = probe_tor_listen();
        if last_ok != Some(st.ok) {
            log_tor_listen_status(&st);
        }
        last_ok = Some(st.ok);
    }
}

fn socks_url_to_host_port(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let rest = trimmed
        .strip_prefix("socks5h://")
        .or_else(|| trimmed.strip_prefix("socks5://"))
        .or_else(|| trimmed.strip_prefix("socks://"))
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
    fn socks_url_parses_default() {
        assert_eq!(
            socks_url_to_host_port("socks5h://127.0.0.1:9050"),
            Some("127.0.0.1:9050".into())
        );
        assert_eq!(
            socks_url_to_host_port("127.0.0.1:9051"),
            Some("127.0.0.1:9051".into())
        );
    }

    #[test]
    fn probe_returns_detail_when_down() {
        // Unlikely ports: expect down in unit test environment.
        std::env::set_var("ITCY_TOR_SOCKS", "socks5h://127.0.0.1:1");
        std::env::set_var("ITCY_TOR_CONTROL", "127.0.0.1:2");
        let st = probe_tor_listen();
        assert!(!st.ok);
        assert!(!st.socks_ok);
        assert!(!st.control_ok);
        assert!(st.detail.contains("make tor-up"));
        std::env::remove_var("ITCY_TOR_SOCKS");
        std::env::remove_var("ITCY_TOR_CONTROL");
    }
}

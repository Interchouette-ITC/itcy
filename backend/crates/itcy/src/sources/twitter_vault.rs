// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Twitter/X Brave gold-profile probe (boot + `GET /status`).
//!
//! Does **not** refuse boot (unlike Ollama warm). No auto-login loop: when cold,
//! operators run `scripts/open-twitter-login.sh` once.

use rusqlite::Connection;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// Snapshot of the login vault used by `fetch-twitter-pulse.sh`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TwitterVaultStatus {
    /// Digest Twitter lane can run: Bearer **or** warm unlocked vault.
    pub ok: bool,
    /// `bearer` | `warm` | `locked` | `cold` | `missing`
    pub mode: String,
    /// Resolved gold path (no secrets).
    pub profile_dir: String,
    /// Human line for logs / operators.
    pub detail: String,
}

/// Resolves gold Brave profile dir (`ITCY_TWITTER_PROFILE_DIR` or `pw/profile-x`).
#[must_use]
pub fn resolve_twitter_gold_dir() -> PathBuf {
    if let Ok(raw) = std::env::var("ITCY_TWITTER_PROFILE_DIR") {
        let t = raw.trim();
        if !t.is_empty() {
            return PathBuf::from(t);
        }
    }
    crate::paths::product_join("pw/profile-x")
}

/// Probe vault + optional Bearer (no network, no browser).
#[must_use]
pub fn probe_twitter_vault() -> TwitterVaultStatus {
    let profile_dir = resolve_twitter_gold_dir();
    let bearer = crate::sources::twitter::load_twitter_creds().is_ok_and(|c| c.has_bearer());
    if bearer {
        return TwitterVaultStatus {
            ok: true,
            mode: "bearer".into(),
            profile_dir: profile_dir.display().to_string(),
            detail: "ok (bearer)".into(),
        };
    }
    if !profile_dir.is_dir() {
        return TwitterVaultStatus {
            ok: false,
            mode: "missing".into(),
            profile_dir: profile_dir.display().to_string(),
            detail: "profile missing (scripts/open-twitter-login.sh)".into(),
        };
    }
    if profile_dir.join("SingletonLock").exists() {
        return TwitterVaultStatus {
            ok: false,
            mode: "locked".into(),
            profile_dir: profile_dir.display().to_string(),
            detail: "locked (close Brave on pw/profile-x with window X)".into(),
        };
    }
    if cookies_look_warm(&profile_dir.join("Default/Cookies")) {
        return TwitterVaultStatus {
            ok: true,
            mode: "warm".into(),
            profile_dir: profile_dir.display().to_string(),
            detail: "ok".into(),
        };
    }
    TwitterVaultStatus {
        ok: false,
        mode: "cold".into(),
        profile_dir: profile_dir.display().to_string(),
        detail: "cold session (scripts/open-twitter-login.sh once; close with window X)".into(),
    }
}

/// Logs vault status once (`info` when ok, `warn` when not).
pub fn log_twitter_vault_status(st: &TwitterVaultStatus) {
    if st.ok {
        info!(
            mode = %st.mode,
            profile = %st.profile_dir,
            "twitter: vault ready"
        );
    } else {
        warn!(
            mode = %st.mode,
            profile = %st.profile_dir,
            detail = %st.detail,
            "twitter: vault not ready"
        );
    }
}

fn cookies_look_warm(cookies_path: &Path) -> bool {
    if !cookies_path.is_file() {
        return false;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp =
        std::env::temp_dir().join(format!("itcy-tw-cookies-{}-{}", std::process::id(), nanos));
    let _ = std::fs::create_dir_all(&tmp);
    let copy = tmp.join("Cookies");
    let warm = (|| {
        std::fs::copy(cookies_path, &copy).ok()?;
        if let Some(parent) = cookies_path.parent() {
            for side in ["Cookies-journal", "Cookies-wal"] {
                let src = parent.join(side);
                if src.is_file() {
                    let _ = std::fs::copy(&src, tmp.join(side));
                }
            }
        }
        let con = Connection::open_with_flags(
            &copy,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        let mut stmt = con
            .prepare(
                "SELECT name FROM cookies WHERE host_key LIKE '%x.com%' OR host_key LIKE '%twitter%'",
            )
            .ok()?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).ok()?;
        let mut has_auth = false;
        let mut has_ct0 = false;
        for name in rows.flatten() {
            if name == "auth_token" {
                has_auth = true;
            }
            if name == "ct0" {
                has_ct0 = true;
            }
        }
        Some(has_auth && has_ct0)
    })()
    .unwrap_or(false);
    let _ = std::fs::remove_dir_all(&tmp);
    warm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_profile_probe() {
        std::env::set_var(
            "ITCY_TWITTER_PROFILE_DIR",
            "/tmp/itcy-twitter-vault-missing-xyz",
        );
        let st = probe_twitter_vault();
        assert_eq!(st.mode, "missing");
        assert!(!st.ok);
        std::env::remove_var("ITCY_TWITTER_PROFILE_DIR");
    }

    #[test]
    fn warm_live_profile_when_present() {
        let gold = crate::paths::product_join("pw/profile-x");
        if !gold.is_dir() {
            return;
        }
        std::env::set_var("ITCY_TWITTER_PROFILE_DIR", gold.as_os_str());
        let st = probe_twitter_vault();
        std::env::remove_var("ITCY_TWITTER_PROFILE_DIR");
        // Live machine may be warm, locked, or cold; just ensure probe returns a mode.
        assert!(
            ["bearer", "warm", "locked", "cold", "missing"].contains(&st.mode.as_str()),
            "unexpected mode {}",
            st.mode
        );
    }
}

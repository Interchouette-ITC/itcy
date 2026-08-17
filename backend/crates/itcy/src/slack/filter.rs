// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Channel allowlist: only the configured `#itcy` id.

/// Returns true when `channel_id` equals the configured allowlist id.
#[must_use]
pub fn is_channel_allowed(allowed_channel_id: &str, channel_id: &str) -> bool {
    !allowed_channel_id.is_empty() && allowed_channel_id == channel_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_exact_match() {
        assert!(is_channel_allowed("C123", "C123"));
    }

    #[test]
    fn denies_other_channel() {
        assert!(!is_channel_allowed("C123", "C999"));
    }

    #[test]
    fn denies_when_allowlist_empty() {
        assert!(!is_channel_allowed("", "C123"));
    }
}

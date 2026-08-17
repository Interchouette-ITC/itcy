// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Slack notice when the always-on binary finishes boot.

use crate::llm::sanitize::sanitize_itcy_text;

/// Operator-facing line posted to `#itcy` after HTTP listen succeeds.
#[must_use]
pub fn boot_ready_text(bind: &str, linkedin_mode: &str, x_mode: &str) -> String {
    sanitize_itcy_text(&format!(
        "ITCy restarted and is listening on `{bind}`.\n\
         LinkedIn: {linkedin_mode} | X: {x_mode} 🦉"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::sanitize::EM_DASH;

    #[test]
    fn boot_ready_has_bind_modes_and_owl() {
        let text = boot_ready_text("127.0.0.1:4700", "playground", "production");
        assert!(text.contains("127.0.0.1:4700"));
        assert!(text.contains("restarted"));
        assert!(text.contains("playground"));
        assert!(text.contains("production"));
        assert!(text.contains('🦉'));
        assert!(!text.contains(EM_DASH));
        assert!(!text.contains(":owl:"));
    }
}

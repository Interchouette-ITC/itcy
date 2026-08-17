// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Freeform message side-paths. Workflows and corpus growth use slash commands.

/// Operator intent for freeform channel text (not slash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Fall through to freeform chat.
    Chat,
}

/// Parses freeform intent. Corpus ingest is slash-only (`/enrich`, `/ingest`).
#[must_use]
pub fn detect_intent(text: &str) -> Intent {
    let _ = text.trim();
    Intent::Chat
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freeform_is_always_chat() {
        assert_eq!(
            detect_intent("please save https://example.com/a"),
            Intent::Chat
        );
        assert_eq!(detect_intent("draft about rust async"), Intent::Chat);
        assert_eq!(detect_intent("submit bat"), Intent::Chat);
        assert_eq!(detect_intent("how is the health endpoint?"), Intent::Chat);
    }
}

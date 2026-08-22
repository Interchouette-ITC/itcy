// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Digest `/propose_draft` and `/propose_tweet` batch layout for Slack.

/// One header plus thread replies (inject/e2e still uses [`Self::summary`]).
pub(crate) struct ProposeBatch {
    pub header: String,
    pub items: Vec<String>,
    pub summary: String,
}

impl ProposeBatch {
    #[must_use]
    pub fn new(header: String, items: Vec<String>) -> Self {
        let summary = if items.is_empty() {
            header.clone()
        } else {
            format!("{header}\n\n{}", items.join("\n\n"))
        };
        Self {
            header,
            items,
            summary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProposeBatch;

    #[test]
    fn batch_summary_joins_items_for_inject() {
        let batch = ProposeBatch::new(
            "From `DIGEST-1`: starting 2 draft(s):".into(),
            vec![
                "--- item 2 ---\ndraft-a".into(),
                "--- item 5 ---\ndraft-b".into(),
            ],
        );
        assert_eq!(batch.items.len(), 2);
        assert!(batch.summary.contains("--- item 2 ---"));
        assert!(batch.summary.contains("--- item 5 ---"));
        assert!(batch.summary.contains("draft-b"));
    }
}

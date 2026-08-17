// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! `corpus_search` over `SQLite` RAG (personal `LinkedIn` / sources bank).

use crate::llm::client::LlmError;
use crate::sources::embed::EmbedClient;
use crate::sources::rag::retrieve_for_subject;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;

pub struct CorpusSearch {
    db_path: PathBuf,
    embed: Arc<dyn EmbedClient>,
}

impl CorpusSearch {
    #[must_use]
    pub fn new(db_path: PathBuf, embed: Arc<dyn EmbedClient>) -> Self {
        Self { db_path, embed }
    }

    /// # Errors
    ///
    /// Returns an [`LlmError`] variant for provider, tool, or empty-content failure.
    pub async fn search(&self, arguments: &str) -> Result<String, LlmError> {
        let v: serde_json::Value =
            serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!({}));
        let query = v
            .get("query")
            .and_then(|q| q.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                LlmError::ToolProvider("corpus_search requires {\"query\": \"...\"}".into())
            })?;
        let result = match retrieve_for_subject(&self.db_path, self.embed.as_ref(), query, 5).await
        {
            Ok(chunks) => {
                let mut out = String::from(
                    "Corpus hits (VOICE / HISTORY only - already ingested).\n\
Do NOT browse_url any URL from this block. LinkedIn URLs are redacted. \
Research cites come from web_search EXTRACTED publisher links only.\n",
                );
                for (i, c) in chunks.iter().enumerate() {
                    let text = redact_linkedin_urls(&c.text);
                    let _ = write!(
                        out,
                        "\n[{}] subject={} score={:.3}\n{}\n",
                        i + 1,
                        c.subject,
                        c.score,
                        text
                    );
                }
                Ok(out)
            }
            Err(e) => Ok(format!("No corpus hits for `{query}`: {e}")),
        };
        result
    }
}

/// Strip linkedin.com / lnkd.in tokens so the writer cannot feed them to `browse_url`.
fn redact_linkedin_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find("http") {
        out.push_str(&rest[..idx]);
        let tail = &rest[idx..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == ')' || c == ']' || c == '<' || c == '|')
            .unwrap_or(tail.len());
        let url = &tail[..end];
        let lower = url.to_ascii_lowercase();
        if crate::sources::url_hygiene::is_linkedin_host(&lower) {
            out.push_str("[linkedin-url-redacted]");
        } else {
            out.push_str(url);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::redact_linkedin_urls;

    #[test]
    fn redacts_linkedin_and_keeps_publisher() {
        let s = "See https://www.linkedin.com/posts/foo_bar/ and https://labs.sogeti.com/x";
        let out = redact_linkedin_urls(s);
        assert!(out.contains("[linkedin-url-redacted]"));
        assert!(!out.to_ascii_lowercase().contains("linkedin.com/posts"));
        assert!(out.contains("https://labs.sogeti.com/x"));
    }

    #[test]
    fn redacts_lnkd_in() {
        let out = redact_linkedin_urls("short https://lnkd.in/abc end");
        assert!(out.contains("[linkedin-url-redacted]"));
        assert!(!out.contains("lnkd.in"));
    }
}

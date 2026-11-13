// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Lossless, sentence-aware X layout. Invalid content is an error, never a clipped post.

use crate::llm::prose::sentence_ends;

pub const X_CHAR_LIMIT: usize = 280;

/// Twitter-text v3 codepoint weights, NFC normalization, emoji sequences, and t.co URLs.
///
/// # Panics
///
/// Panics if the parser violates its nonnegative weighted-length invariant.
#[must_use]
pub fn x_weighted_len(text: &str) -> usize {
    // The Nom backend undercounts bare domains such as crates.io; keep the validated backend.
    usize::try_from(
        twitter_text::parse_with_parser_backend(
            text,
            twitter_text_config::config_v3(),
            true,
            twitter_text::ParserBackend::External,
        )
        .weighted_length,
    )
    .expect("Twitter-text length is nonnegative")
}

#[must_use]
pub fn fits_x_limit(text: &str) -> bool {
    x_weighted_len(text) <= X_CHAR_LIMIT
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ThreadLayoutError {
    #[error("empty tweet after footer removal")]
    Empty,
    #[error("tweet contains invalid control characters")]
    InvalidCharacters,
    #[error("cannot fit {weighted} weighted characters into two complete tweets (280 each); shorten the commentary or a sentence, never truncate")]
    CannotPartition { weighted: usize },
}

/// Plan one or two posts by slicing the input at complete sentence boundaries.
///
/// The caller supplies final public text, including all handles, links and tags.
/// Only whitespace at a tweet boundary and explicit operator thread chrome are removed.
///
/// # Errors
///
/// Rejects empty/invalid text or a payload without a complete two-post partition.
pub fn layout_x_thread(text: &str) -> Result<Vec<String>, ThreadLayoutError> {
    let text = strip_thread_marks(text);
    let text = text.trim();
    if text.is_empty() {
        return Err(ThreadLayoutError::Empty);
    }
    if text.chars().any(|c| {
        (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
            || matches!(c, '\u{fffe}' | '\u{feff}' | '\u{ffff}')
    }) {
        return Err(ThreadLayoutError::InvalidCharacters);
    }
    let weight = x_weighted_len(text);
    if weight <= X_CHAR_LIMIT {
        return Ok(vec![text.to_string()]);
    }
    let mut boundaries = sentence_ends(text);
    // Hashtag / link trailer is allowed to be a complete second tweet.
    for (offset, _) in text.match_indices('\n') {
        let tail = text[offset..].trim();
        if tail.starts_with('#') || tail.starts_with("https://") {
            boundaries.push(offset);
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut candidates: Vec<(bool, usize, usize)> = Vec::new();
    for boundary in boundaries {
        let root = text[..boundary].trim_end();
        let reply = text[boundary..].trim_start();
        if root.is_empty() || reply.is_empty() {
            continue;
        }
        let a = x_weighted_len(root);
        let b = x_weighted_len(reply);
        if a <= X_CHAR_LIMIT && b <= X_CHAR_LIMIT {
            let paragraph =
                text[boundary..].starts_with("\n\n") || text[..boundary].ends_with("\n\n");
            candidates.push((!paragraph, a.abs_diff(b), boundary));
        }
    }
    candidates.sort_unstable();
    if let Some((_, _, boundary)) = candidates.first() {
        return Ok(vec![
            text[..*boundary].trim_end().to_string(),
            text[*boundary..].trim_start().to_string(),
        ]);
    }
    Err(ThreadLayoutError::CannotPartition { weighted: weight })
}

#[must_use]
pub fn strip_thread_marks(text: &str) -> String {
    text.lines()
        .filter(|line| !is_thread_chrome_line(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[must_use]
pub fn is_thread_chrome_line(line: &str) -> bool {
    let text = line.trim();
    if text == "--- thread ---" {
        return true;
    }
    let text = text.trim_matches(['(', ')']);
    let parts: Vec<&str> = text.split('/').collect();
    parts.len() == 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "🦉 GitHub Models' retirement feels like a quiet end to a promising experiment.\nSad to see such a tool fade-especially when free alternatives are scarce.\n\nMicrosoft Foundry and Copilot?\nNot exactly the open-source dream we hoped for.\n\nBuilders, keep an eye on migration paths.\nThe future of AI tools is still in flux.\n\n#AI #GitHub #ModelRetirement\n\nhttps://blog.dante.company/en/articles/github-models-retirement-migration-2026-07-02";
    const AGENTPAY: &str = "📜 CSPR AgentPay Guard is the firewall before an AI agent pays, HTTP 402 rules, allowlists, and replay protection all in one. 🚀\n\nIt's not just about spending limits, it's about securing the whole flow: budget, expiry, audit trails, and even mock local tests with real Casper Testnet proof. 🐚\n\nMVP, not production custody. But if you're building on Casper, this is your guardrail. 🔐\n\n#CSPR #AgentPay #OnChain #AI\n\nhttps://alsaecas.dev/projects/cspr-agentpay-guard";

    fn assert_lossless(raw: &str, parts: &[String]) {
        assert!((1..=2).contains(&parts.len()));
        assert!(parts.iter().all(|p| !p.is_empty() && fits_x_limit(p)));
        assert_eq!(
            parts.join("\n\n").split_whitespace().collect::<Vec<_>>(),
            raw.split_whitespace().collect::<Vec<_>>()
        );
    }

    #[test]
    fn sample_splits_losslessly_and_deterministically() {
        let parts = layout_x_thread(SAMPLE).expect("two complete posts");
        assert_eq!(parts.len(), 2);
        assert_lossless(SAMPLE, &parts);
        assert_eq!(layout_x_thread(SAMPLE).unwrap(), parts);
    }

    #[test]
    fn agentpay_is_rejected_instead_of_forcing_a_word_split() {
        // The former test demanded a near-280 root cutting the second sentence.
        assert!(matches!(
            layout_x_thread(AGENTPAY),
            Err(ThreadLayoutError::CannotPartition { .. })
        ));
    }

    #[test]
    fn no_trailer_never_drops_content() {
        let text = "Builders keep an eye on migration paths. ".repeat(10);
        assert_lossless(&text, &layout_x_thread(&text).unwrap());
        assert!(layout_x_thread(&"x".repeat(281)).is_err());
        assert!(layout_x_thread(&"Builders keep an eye on migration paths. ".repeat(20)).is_err());
    }

    #[test]
    fn exact_weight_boundaries() {
        for n in [279, 280] {
            let text = "a".repeat(n);
            assert_eq!(layout_x_thread(&text).unwrap(), vec![text]);
        }
        assert!(layout_x_thread(&"a".repeat(281)).is_err());
        let text = format!("{}.\n\n{}.", "a".repeat(279), "b".repeat(279));
        assert_lossless(&text, &layout_x_thread(&text).unwrap());
        let text = format!("{}.\n\n{}.", "a".repeat(280), "b".repeat(279));
        assert!(layout_x_thread(&text).is_err());
    }

    #[test]
    fn reference_weight_vectors() {
        for (text, weight) in [
            ("hello", 5),
            ("日本語", 6),
            ("e\u{301}", 1),
            ("☕️", 2),
            ("👩🏽‍💻", 2),
            ("🇫🇷", 2),
            ("1️⃣", 2),
            ("👨‍👩‍👧‍👦", 2),
            ("crates.io", 23),
            ("hi https://example.com/x zz", 29),
            ("v1.2.3", 6),
            ("@example.com", 12),
        ] {
            assert_eq!(x_weighted_len(text), weight, "{text}");
        }
    }

    #[test]
    fn whole_sentences_not_word_suffixes() {
        let text = format!(
            "{}.\n\nThe object refs remain intact.\n\n#Rust\n\nhttps://example.com/x",
            "a".repeat(250)
        );
        let parts = layout_x_thread(&text).unwrap();
        assert_eq!(
            parts[1],
            "The object refs remain intact.\n\n#Rust\n\nhttps://example.com/x"
        );
        assert_lossless(&text, &parts);
    }

    #[test]
    fn bounded_sentence_matrix_conserves_every_token() {
        for a in [40, 100, 180, 260, 281] {
            for b in [40, 100, 180, 260, 281] {
                for trailer in ["", "\n\n#Rust #AI\n\nhttps://example.com/a"] {
                    let text = format!("{}.\n\n{}.{trailer}", "a".repeat(a), "b".repeat(b));
                    if let Ok(parts) = layout_x_thread(&text) {
                        assert_lossless(&text, &parts);
                    }
                }
            }
        }
    }

    #[test]
    fn thread_chrome_only_is_removed() {
        assert_eq!(
            layout_x_thread("1/2\nHello.\n\n--- thread ---\n2/2\n#Rust").unwrap(),
            vec!["Hello.\n\n#Rust"]
        );
        assert_eq!(layout_x_thread(""), Err(ThreadLayoutError::Empty));
        assert_eq!(
            layout_x_thread("hi\0"),
            Err(ThreadLayoutError::InvalidCharacters)
        );
    }
}

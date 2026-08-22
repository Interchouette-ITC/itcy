// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Transcript-exact fixtures for `/propose_draft` regression tests (Cases A-D).

use crate::sources::digest::{digest_propose_brief, DigestItem};

pub const FIXTURE_A_BAD_BODY: &str =
    "A supply chain attack on Rust crates is putting build-time malware \
into developer machines. The Rust Language Server Protocol (LSP) tools like Rust Analyzer are a \
major attack vector with 245 million downloads at risk.";

pub const FIXTURE_B_BAD_BODY: &str =
    "OxiSH is a memory-safe SSH server project built in Rust as an \
alternative to OpenSSH. The OpenSSH ecosystem needs careful review when new SSH server projects \
land in the forge.";

pub const FIXTURE_C_BAD_BODY: &str =
    "The corpus search returned hits. I will write a warm LinkedIn-style \
commentary about aws-bench based on what I know from the operator subject.";

pub const FIXTURE_D_BAD_BODY: &str =
    "The article from InfoWorld highlights a growing concern around Anthropic's Opus language model \
and its potential impact on AI coding workflows.";

#[must_use]
pub fn fixture_a_item() -> DigestItem {
    DigestItem {
        idx: 40,
        title: "ayush @ayushagarwal027 · Came across a Rust LSP".into(),
        url: Some("https://x.com/ayushagarwal027/status/2090736100025504071".into()),
        subject: "Came across a Rust LSP that stays under".into(),
        lane: "for_you".into(),
        weight: 1,
        detail: "Came across a Rust LSP that stays under 100MB of RAM, and instantly resumes indexing after restart.\n\nRust Glancer is a 4-month-old alternative to rust-analyzer.\n\nThe motivation was".into(),
    }
}

#[must_use]
pub fn fixture_b_item() -> DigestItem {
    DigestItem {
        idx: 2,
        title: "Open weights reshape vendor lock-in".into(),
        url: Some(
            "https://www.infoworld.com/article/4212345/open-weights-frontier-models-vendor-lock-in.html"
                .into(),
        ),
        subject: "open weights benefit vendors frontier models".into(),
        lane: "live_site".into(),
        weight: 1,
        detail: "Open weights benefit vendors building on frontier models without getting locked in.\n\nThe shift changes who controls the stack.".into(),
    }
}

#[must_use]
pub fn fixture_c_item() -> DigestItem {
    DigestItem {
        idx: 17,
        title: "AWS aws-bench agent evaluation".into(),
        url: Some("https://www.infoq.com/news/2026/08/aws-bench-agent-evaluation".into()),
        subject: "aws releases aws-bench evaluate agents cloud".into(),
        lane: "live_site".into(),
        weight: 1,
        detail: "AWS has released aws-bench, an open-source benchmark for evaluating AI agents on real AWS tasks such as misconfigurations and infrastructure provisioning.".into(),
    }
}

#[must_use]
pub fn fixture_d_item() -> DigestItem {
    DigestItem {
        idx: 15,
        title: "Anthropic Opus language problems".into(),
        url: Some("https://www.infoworld.com/article/4211958/anthropics-opus-language-problems-may-be-creating-a-hidden-cost-for-ai-coding.html".into()),
        subject: "anthropic opus language problems hidden cost ai coding".into(),
        lane: "live_site".into(),
        weight: 1,
        detail: "Anthropic's Opus language problems may be creating a hidden cost for AI coding workflows.".into(),
    }
}

#[must_use]
pub fn fixture_a_brief() -> String {
    digest_propose_brief(&fixture_a_item()).1
}

#[must_use]
pub fn fixture_b_brief() -> String {
    digest_propose_brief(&fixture_b_item()).1
}

#[must_use]
pub fn fixture_c_brief() -> String {
    digest_propose_brief(&fixture_c_item()).1
}

#[must_use]
pub fn fixture_d_brief() -> String {
    digest_propose_brief(&fixture_d_item()).1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_a_brief_contains_glancer_and_x_url() {
        let brief = fixture_a_brief();
        assert!(brief.contains("Rust Glancer"));
        assert!(brief.contains("2090736100025504071"));
    }

    #[test]
    fn fixture_b_brief_contains_open_weights_and_infoworld() {
        let brief = fixture_b_brief();
        assert!(brief.to_ascii_lowercase().contains("open weights"));
        assert!(brief.contains("infoworld.com"));
    }

    #[test]
    fn fixture_c_brief_aws_bench_infoq() {
        let brief = fixture_c_brief();
        assert!(brief.to_ascii_lowercase().contains("aws-bench"));
        assert!(brief.contains("infoq.com"));
    }

    #[test]
    fn fixture_d_brief_opus_infoworld() {
        let brief = fixture_d_brief();
        assert!(brief.to_ascii_lowercase().contains("opus"));
        assert!(brief.contains("infoworld.com"));
    }

    #[test]
    fn fixture_bad_bodies_match_transcript_snippets() {
        assert!(FIXTURE_A_BAD_BODY
            .to_ascii_lowercase()
            .contains("supply chain"));
        assert!(FIXTURE_B_BAD_BODY.to_ascii_lowercase().contains("oxish"));
        assert!(FIXTURE_C_BAD_BODY
            .to_ascii_lowercase()
            .contains("corpus search returned"));
        assert!(FIXTURE_D_BAD_BODY.contains("InfoWorld"));
        assert!(!FIXTURE_D_BAD_BODY.contains("@infoworld"));
    }
}

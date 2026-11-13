// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

use itcy::sources::{
    draft_footer::aerate_linkedin_draft, tweet_footer::aerate_tweet_commentary,
    tweet_thread::x_weighted_len,
};

const OPENCRABS: &str = "OpenCrabs folds OpenClaw's agent magic into one secure, local binary 🔐 No external listeners, RAM keys zeroized on drop, and self-healing loops ready to keep you working even when things get tricky 🦀 Just compile, TUI up, and let the Rust daemon manage files and research while you sip coffee ☕\n\n🦉 #OpenCrabs #Rust #LocalAI\n\nhttps://hysenlabs.com/projects/adolfousier-opencrabs";
const RUNSYBIL: &str = "A chat template file that shouldn't be trusted? Researchers just showed RunSybil using a malicious `chat_template.jinja` to break CPython's Jinja sandbox on vLLM 🤖📜 Python's team notes sandbox escapes fall outside its threat model, but the memory-safety flaw leaking object refs is wild. The patch landed in 3.13+; 3.12 stays stuck in production for now 🦀\n\n#CPython #AI #SandboxEscape #Security\n\nhttps://www.infoworld.com/article/4245631/cpython-flaw-exposes-ai-inference-servers-to-sandbox-escape-attacks-2.html";

fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

#[test]
fn tweet_216_keeps_every_word_including_coffee() {
    assert!(
        itcy::publish::tweet_texts_for_api(OPENCRABS).is_err(),
        "legacy malformed copy must require rework"
    );
    let normalized = aerate_tweet_commentary(OPENCRABS);
    let parts = itcy::publish::tweet_texts_for_api(&normalized).unwrap();
    assert_eq!(parts.len(), 2);
    assert!(parts.iter().all(|p| x_weighted_len(p) <= 280));
    assert_eq!(
        parts.iter().map(|p| x_weighted_len(p)).collect::<Vec<_>>(),
        [204, 151]
    );
    assert_eq!(words(&parts.join("\n\n")), words(&normalized));
    assert!(parts.join("\n\n").contains("you sip coffee. ☕"));
    let preview = itcy::publish::tweet_preview_for_operator(&normalized).unwrap();
    assert!(preview.contains("Tweet 1/2 (204 / 280 weighted)"));
    assert!(preview.contains("Tweet 2/2 (151 / 280 weighted)"));
    for part in parts {
        assert!(preview.contains(&part));
    }
}

#[test]
fn tweet_221_keeps_filename_and_sentence_boundary() {
    let normalized = aerate_tweet_commentary(RUNSYBIL);
    let parts = itcy::publish::tweet_texts_for_api(&normalized).unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(
        parts.iter().map(|p| x_weighted_len(p)).collect::<Vec<_>>(),
        [167, 256]
    );
    assert!(parts.join("\n\n").contains("`chat_template.jinja`"));
    assert!(!parts[1].starts_with("refs is wild."));
    assert_eq!(words(&parts.join("\n\n")), words(&normalized));
}

#[test]
fn draft_276_punctuates_closing_emoji() {
    let body = "The model fits into a 74.5 GB footprint without demanding its usual VRAM tribute 🚀🦉\n\nTeams building local coding agents gain an alternative path 🤖🧠";
    assert_eq!(
        aerate_linkedin_draft(body),
        "The model fits into a 74.5 GB footprint without demanding its usual VRAM tribute. 🚀🦉\n\nTeams building local coding agents gain an alternative path. 🤖🧠"
    );
}

#[test]
fn draft_273_checks_inside_existing_paragraphs() {
    let body = "The backend requires attention.\n\nConfiguration errors compound instantly. 🚀 A robust platform means maintaining observability.";
    assert_eq!(
        aerate_linkedin_draft(body),
        "The backend requires attention.\n\nConfiguration errors compound instantly. 🚀\n\nA robust platform means maintaining observability."
    );
}

#[test]
fn weighted_emoji_sequences_are_single_glyphs() {
    for glyph in ["☕️", "👩🏽‍💻", "🇫🇷", "1️⃣", "👨‍👩‍👧‍👦"] {
        assert_eq!(x_weighted_len(glyph), 2, "{glyph}");
    }
}

#[test]
fn older_published_regressions_still_preserve_complete_prose() {
    for raw in [
            "📜 Rust devs, meet your new CLI toolkit, a curated crate pack that’s like a 🦀-powered toolbox for your workflow. No more sifting through crates.io chaos. Just swap in the right batteries for your project. 🚀\n\nCargo bp lets you add these packs with `cargo bp add cli`, no more dependency guesswork. It’s about making your stack feel like a 🐦-fueled ride. 🧠\n\n#Rust #CLI #Cargo #Crates\n\nhttps://smallcultfollowing.com/babysteps/blog/2026/07/15/battery-packs",
            "Google’s Gemini AI just did what every Rustacean dreams of: rewriting C/C++ code into safe Rust, catching a memory-safety bug along the way. 🦀⚡\nIt’s not just a rewrite, it’s a proof of concept for scaling memory safety at scale. 🔧\nWho needs a security audit when your AI does it for you? 🤖\n\n#Rust #MemorySafety #AI #OpenSource\n\nhttps://x.com/rustaceans_rs/status/2092134050060275910",
            "It keeps the unified AST format for plugin flexibility but adds native support for GitHub Flavored Markdown,\nsmart punctuation, and more.\n\n🦀 Built in Rust, it's the new default in Astro 7.0, and it's already making waves.\n\n#AI #DevTools #Markdown #Rust\n\nhttps://byteiota.com/astro-64-satteri-rust-markdown",
        ] {
            let normalized = aerate_tweet_commentary(raw);
            let parts = itcy::publish::tweet_texts_for_api(&normalized).unwrap();
            assert!((1..=2).contains(&parts.len()));
            assert!(parts.iter().all(|p| x_weighted_len(p) <= 280));
            assert_eq!(words(&parts.join("\n\n")), words(&normalized));
            assert!(parts.iter().all(|p| !p.starts_with("it for you?")));
            assert!(!parts.join("\n\n").contains("Markdown,\n"));
    }
}

#[tokio::test]
async fn playground_refuses_invalid_or_multi_post_replies_without_success_audit() {
    use itcy::publish::{ship_x_post, PublishAuditStore, PublishMode, XPublishRequest};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("audit.db");
    for (body, parent) in [
        ("a".repeat(281), None),
        (aerate_tweet_commentary(OPENCRABS), Some("123".to_string())),
        (
            "Correct. 🦉\n\nNext paragraph.".to_string(),
            Some("123".to_string()),
        ),
    ] {
        let expected_success = body.starts_with("Correct.");
        let result = ship_x_post(
            &db,
            "playground",
            XPublishRequest {
                tweet_id: Some("TWEET-contract".into()),
                pubs_pr_number: None,
                body,
                quote_tweet_id: None,
                in_reply_to_tweet_id: parent,
            },
            Some(PublishMode::Playground),
        )
        .await;
        assert_eq!(result.is_ok(), expected_success);
        if !expected_success {
            assert!(PublishAuditStore::open(&db)
                .unwrap()
                .latest_ok(Some("TWEET-contract"), None)
                .unwrap()
                .is_none());
        }
    }
}

#[tokio::test]
async fn partial_thread_audit_blocks_a_production_root_retry_before_network() {
    use itcy::publish::{
        ship_x_post, PublishAuditStore, PublishAuditWrite, PublishError, PublishMode,
        PublishRequest, XPublishRequest,
    };
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("audit.db");
    let audit = PublishAuditStore::open(&db).unwrap();
    let body = "A complete root.".to_string();
    let err = PublishError::Other(
        "partial X thread: root=123 url=https://x.com/Interchouette/status/123; reply failed"
            .into(),
    );
    audit
        .insert(&PublishAuditWrite::from_err(
            &PublishRequest {
                draft_id: Some("XPOST-contract".into()),
                pubs_pr_number: None,
                body: body.clone(),
            },
            PublishMode::Production,
            &err,
        ))
        .unwrap();
    let result = ship_x_post(
        &db,
        "production",
        XPublishRequest {
            tweet_id: Some("XPOST-contract".into()),
            pubs_pr_number: None,
            body,
            quote_tweet_id: None,
            in_reply_to_tweet_id: None,
        },
        Some(PublishMode::Production),
    )
    .await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("refusing to duplicate"));
    assert!(audit
        .latest_ok(Some("XPOST-contract"), None)
        .unwrap()
        .is_none());
}

#[test]
fn url_additions_are_validated_after_assembly() {
    use itcy::sources::tweet_footer::apply_change_tweet_url;
    let root = format!("{}.", "a".repeat(279));
    let body = format!("{root}\n\n{}.", "b".repeat(249));
    assert!(itcy::publish::tweet_texts_for_api(&body).is_ok());
    let url = "https://www.rust-lang.org/learn";
    // Include both the separating newlines and the t.co URL weight.
    let out = apply_change_tweet_url("TWEET-contract", &body, &[], url);
    // 250 + 2 + 23 = 275: still valid at the exact public-payload budget.
    let (composed, _) = out.unwrap();
    let parts = itcy::publish::tweet_texts_for_api(&composed).unwrap();
    assert_eq!(x_weighted_len(&parts[1]), 275);
    let oversized = format!("{root}\n\n{}.", "b".repeat(259));
    assert!(apply_change_tweet_url("TWEET-contract", &oversized, &[], url).is_err());
}

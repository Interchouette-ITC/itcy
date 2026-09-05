// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Compile-time writer prompts.
//!
//! `build.rs` copies operator prompt files into `OUT_DIR` when present, otherwise
//! short stubs so the crate still compiles.

macro_rules! prompt {
    ($name:literal) => {
        include_str!(concat!(env!("OUT_DIR"), "/prompts/", $name))
    };
}

/// Cast / identity block shared by load, draft, and freeform.
pub const WHO_IS_WHO: &str = prompt!("who_is_who.md");

/// Disclosed AI CMO role, operator flows, and execution checklist.
pub const AI_CMO: &str = prompt!("ai_cmo.md");

/// Creative studio for X tweets / farce / tweet rework.
pub const CREATIVE_X: &str = prompt!("creative_x.md");

/// Creative studio for `LinkedIn` drafts / draft rework.
pub const CREATIVE_LINKEDIN: &str = prompt!("creative_linkedin.md");

/// Form craft for X tweets / farce / tweet rework.
pub const FORM_CRAFT_X: &str = prompt!("form_craft_x.md");

/// Form craft for `LinkedIn` drafts / draft rework.
pub const FORM_CRAFT_LINKEDIN: &str = prompt!("form_craft_linkedin.md");

/// Load system core (date line prepended at runtime).
pub const LOAD_SYSTEM_CORE: &str = prompt!("load_system.md");

/// Draft writer system core (date line prepended at runtime).
pub const DRAFT_SYSTEM_CORE: &str = prompt!("draft_system.md");

/// Tweet writer system core (date line prepended at runtime).
pub const TWEET_SYSTEM_CORE: &str = prompt!("tweet_system.md");

/// Tweet `/rework` system core (slim; operator override; no write-time subject fact lock).
pub const TWEET_REWORK_SYSTEM_CORE: &str = prompt!("tweet_rework_system.md");

/// Farce tweet writer (`/tweet_farce`); joke voice, no SERP.
pub const TWEET_FARCE_SYSTEM_CORE: &str = prompt!("tweet_farce_system.md");

/// Alias kept for tests that check core rules.
pub const DRAFT_SYSTEM: &str = DRAFT_SYSTEM_CORE;

/// Slack freeform chat system core.
pub const FREEFORM_SYSTEM_CORE: &str = prompt!("freeform_system.md");

const LOAD_USER_TMPL: &str = prompt!("load_user.md");
const DRAFT_USER_TMPL: &str = prompt!("draft_user.md");
const TWEET_USER_TMPL: &str = prompt!("tweet_user.md");
const TWEET_FARCE_USER_TMPL: &str = prompt!("tweet_farce_user.md");
const DRAFT_REWORK_USER_TMPL: &str = prompt!("draft_rework_user.md");
const DRAFT_REWORK_REFRESH_USER_TMPL: &str = prompt!("draft_rework_refresh_user.md");
const TWEET_REWORK_USER_TMPL: &str = prompt!("tweet_rework_user.md");
const TWEET_REWORK_REFRESH_USER_TMPL: &str = prompt!("tweet_rework_refresh_user.md");
const TWEET_REWORK_USER_TOOLS_TMPL: &str = prompt!("tweet_rework_user_tools.md");
const TWEET_REWORK_USER_FARCE_TMPL: &str = prompt!("tweet_rework_user_farce.md");
const REWORK_EMPTY_PACK_TMPL: &str = prompt!("rework_empty_pack.md");
const TWEET_REWORK_COMMENTARY_EXPLODED: &str = prompt!("tweet_rework_commentary_exploded.md");
const TWEET_REWORK_COMMENTARY_EMPTY: &str = prompt!("tweet_rework_commentary_empty.md");
const TWEET_PACK_NOTE_SUBJECT_HTTPS: &str = prompt!("tweet_pack_note_subject_https.md");
const TWEET_PACK_NOTE_EMPTY: &str = prompt!("tweet_pack_note_empty.md");
const TWEET_PACK_NOTE_NORMAL: &str = prompt!("tweet_pack_note_normal.md");
const DRAFT_PACK_NOTE_EMPTY: &str = prompt!("draft_pack_note_empty.md");
const DRAFT_PACK_NOTE_NORMAL: &str = prompt!("draft_pack_note_normal.md");
const DRAFT_PACK_NOTE_SUBJECT_HTTPS: &str = prompt!("draft_pack_note_subject_https.md");
const DRAFT_USER_SUBJECT_HTTPS_TMPL: &str = prompt!("draft_user_subject_https.md");

/// Self-introduction writer system core (date line prepended at runtime).
pub const SELF_SYSTEM_CORE: &str = prompt!("self_system.md");
pub const DRAFT_REWORK_SYSTEM_CORE: &str = prompt!("draft_rework_system.md");
/// `DRAFT-` `/rework` with no instructions (refresh / redo).
pub const DRAFT_REWORK_REFRESH_SYSTEM_CORE: &str = prompt!("draft_rework_refresh_system.md");
/// `TWEET-` `/rework` with no instructions (refresh / redo).
pub const TWEET_REWORK_REFRESH_SYSTEM_CORE: &str = prompt!("tweet_rework_refresh_system.md");

/// `LinkedIn` comment-reply system (`/accept_comment_reply`).
pub const COMMENT_REPLY_SYSTEM_CORE: &str = prompt!("comment_reply_system.md");

/// X tweet-reply system (`/accept_tweet_reply`).
pub const TWEET_REPLY_SYSTEM_CORE: &str = prompt!("tweet_reply_system.md");

/// `CREPLY-` `/rework` with no instructions (refresh).
pub const REPLY_REWORK_REFRESH_SYSTEM_CORE: &str = prompt!("reply_rework_refresh_system.md");
/// `CREPLY-` `/rework` with edit instructions.
pub const REPLY_REWORK_INSTRUCTION_SYSTEM_CORE: &str =
    prompt!("reply_rework_instruction_system.md");
/// `XREPLY-` `/rework` with no instructions (refresh).
pub const TWEET_REPLY_REWORK_REFRESH_SYSTEM_CORE: &str =
    prompt!("tweet_reply_rework_refresh_system.md");
/// `XREPLY-` `/rework` with edit instructions.
pub const TWEET_REPLY_REWORK_INSTRUCTION_SYSTEM_CORE: &str =
    prompt!("tweet_reply_rework_instruction_system.md");

const SELF_USER_TMPL: &str = prompt!("self_user.md");
const COMMENT_REPLY_USER_TMPL: &str = prompt!("comment_reply_user.md");
const TWEET_REPLY_USER_TMPL: &str = prompt!("tweet_reply_user.md");
const REPLY_REWORK_REFRESH_USER_TMPL: &str = prompt!("reply_rework_refresh_user.md");
const REPLY_REWORK_INSTRUCTION_USER_TMPL: &str = prompt!("reply_rework_instruction_user.md");
const TWEET_REPLY_REWORK_REFRESH_USER_TMPL: &str = prompt!("tweet_reply_rework_refresh_user.md");
const TWEET_REPLY_REWORK_INSTRUCTION_USER_TMPL: &str =
    prompt!("tweet_reply_rework_instruction_user.md");

fn fill(tmpl: &str, key: &str, value: &str) -> String {
    tmpl.replace(&format!("{{{key}}}"), value)
}

/// LOAD user turn for a subject.
#[must_use]
pub fn load_user_message(subject: &str) -> String {
    LOAD_USER_TMPL.replace(concat!("{", "subject", "}"), subject)
}

/// DRAFT user turn after `ResearchPack` is ready.
#[must_use]
pub fn draft_user_message(research_pack: &str, pack_note: &str, subject: &str) -> String {
    draft_user_message_inner(DRAFT_USER_TMPL, research_pack, pack_note, subject)
}

/// Cite-locked draft user turn: no corpus/browse/search instructions.
#[must_use]
pub fn draft_user_message_subject_https(
    research_pack: &str,
    pack_note: &str,
    subject: &str,
) -> String {
    draft_user_message_inner(
        DRAFT_USER_SUBJECT_HTTPS_TMPL,
        research_pack,
        pack_note,
        subject,
    )
}

fn draft_user_message_inner(
    tmpl: &str,
    research_pack: &str,
    pack_note: &str,
    subject: &str,
) -> String {
    tmpl.replace(concat!("{", "research_pack", "}"), research_pack)
        .replace(concat!("{", "pack_note", "}"), pack_note)
        .replace(concat!("{", "subject", "}"), subject)
}

/// Tweet writer user turn after `ResearchPack` is ready.
#[must_use]
pub fn tweet_user_message(research_pack: &str, pack_note: &str, subject: &str) -> String {
    TWEET_USER_TMPL
        .replace(concat!("{", "research_pack", "}"), research_pack)
        .replace(concat!("{", "pack_note", "}"), pack_note)
        .replace(concat!("{", "subject", "}"), subject)
}

/// Farce tweet user turn (`theme` may be empty).
#[must_use]
pub fn tweet_farce_user_message(theme: &str) -> String {
    let theme = if theme.trim().is_empty() {
        "(none - pick a fresh gag)"
    } else {
        theme.trim()
    };
    TWEET_FARCE_USER_TMPL.replace(concat!("{", "theme", "}"), theme)
}

/// Self-introduction user turn (`surface` is `"linkedin"` or `"x"`; `instructions` may be empty).
#[must_use]
pub fn self_user_message(surface: &str, instructions: &str) -> String {
    let mut s = fill(SELF_USER_TMPL, "surface", surface);
    s = fill(&s, "instructions", instructions.trim());
    s
}

/// `/accept_comment_reply` user turn (full parent post + comment).
#[must_use]
pub fn comment_reply_user_message(
    parent_post: &str,
    comment_author: &str,
    comment_body: &str,
) -> String {
    let mut s = fill(COMMENT_REPLY_USER_TMPL, "parent_post", parent_post);
    s = fill(&s, "comment_author", comment_author);
    fill(&s, "comment_body", comment_body)
}

/// `/accept_tweet_reply` user turn (parent tweet author + body).
#[must_use]
pub fn tweet_reply_user_message(tweet_author: &str, tweet_body: &str) -> String {
    let s = fill(TWEET_REPLY_USER_TMPL, "tweet_author", tweet_author);
    fill(&s, "tweet_body", tweet_body)
}

/// `CREPLY-` refresh `/rework` user turn (no instructions).
#[must_use]
pub fn reply_rework_refresh_user_message(
    parent_post: &str,
    comment_author: &str,
    comment_body: &str,
    prior: &str,
) -> String {
    let mut s = fill(REPLY_REWORK_REFRESH_USER_TMPL, "parent_post", parent_post);
    s = fill(&s, "comment_author", comment_author);
    s = fill(&s, "comment_body", comment_body);
    fill(&s, "prior", prior)
}

/// `CREPLY-` instruction `/rework` user turn.
#[must_use]
pub fn reply_rework_instruction_user_message(
    instructions: &str,
    parent_post: &str,
    comment_author: &str,
    comment_body: &str,
    prior: &str,
    ban_block: &str,
) -> String {
    let mut s = fill(
        REPLY_REWORK_INSTRUCTION_USER_TMPL,
        "instructions",
        instructions.trim(),
    );
    s = fill(&s, "parent_post", parent_post);
    s = fill(&s, "comment_author", comment_author);
    s = fill(&s, "comment_body", comment_body);
    s = fill(&s, "prior", prior);
    fill(&s, "ban_block", ban_block)
}

/// `XREPLY-` refresh `/rework` user turn (no instructions).
#[must_use]
pub fn tweet_reply_rework_refresh_user_message(
    tweet_author: &str,
    tweet_body: &str,
    prior: &str,
) -> String {
    let mut s = fill(
        TWEET_REPLY_REWORK_REFRESH_USER_TMPL,
        "tweet_author",
        tweet_author,
    );
    s = fill(&s, "tweet_body", tweet_body);
    fill(&s, "prior", prior)
}

/// `XREPLY-` instruction `/rework` user turn.
#[must_use]
pub fn tweet_reply_rework_instruction_user_message(
    instructions: &str,
    tweet_author: &str,
    tweet_body: &str,
    prior: &str,
    ban_block: &str,
) -> String {
    let mut s = fill(
        TWEET_REPLY_REWORK_INSTRUCTION_USER_TMPL,
        "instructions",
        instructions.trim(),
    );
    s = fill(&s, "tweet_author", tweet_author);
    s = fill(&s, "tweet_body", tweet_body);
    s = fill(&s, "prior", prior);
    fill(&s, "ban_block", ban_block)
}

/// Empty `ResearchPack` stub when a rework has no stored pack.
#[must_use]
pub fn rework_empty_pack(subject: &str) -> String {
    fill(REWORK_EMPTY_PACK_TMPL, "subject", subject)
}

/// `/rework` user turn for a `DRAFT-` id.
#[must_use]
pub fn draft_rework_user_message(
    instructions: &str,
    id: &str,
    subject: &str,
    pack: &str,
    body: &str,
    url_lock: &str,
) -> String {
    let mut s = fill(DRAFT_REWORK_USER_TMPL, "instructions", instructions.trim());
    s = fill(&s, "id", id);
    s = fill(&s, "subject", subject);
    s = fill(&s, "pack", pack);
    s = fill(&s, "body", body);
    fill(&s, "url_lock", url_lock)
}

/// Draft `/rework` refresh (empty instructions) user turn.
#[must_use]
pub fn draft_rework_refresh_user_message(
    id: &str,
    subject: &str,
    pack: &str,
    body: &str,
    url_lock: &str,
    ban_block: &str,
) -> String {
    let mut s = fill(DRAFT_REWORK_REFRESH_USER_TMPL, "id", id);
    s = fill(&s, "subject", subject);
    s = fill(&s, "pack", pack);
    s = fill(&s, "body", body);
    s = fill(&s, "url_lock", url_lock);
    fill(&s, "ban_block", ban_block)
}

/// Inputs for [`tweet_rework_user_message`].
pub struct TweetReworkUserArgs<'a> {
    pub instructions: &'a str,
    pub id: &'a str,
    pub subject: &'a str,
    pub commentary: &'a str,
    pub cite: &'a str,
    pub pack: &'a str,
    pub farce: bool,
    pub needs_tools: bool,
}

/// Tweet `/rework` user turn (default rewrite / tools / farce pick a full template).
#[must_use]
pub fn tweet_rework_user_message(args: &TweetReworkUserArgs<'_>) -> String {
    let tmpl = if args.farce {
        TWEET_REWORK_USER_FARCE_TMPL
    } else if args.needs_tools {
        TWEET_REWORK_USER_TOOLS_TMPL
    } else {
        TWEET_REWORK_USER_TMPL
    };
    let mut s = fill(tmpl, "instructions", args.instructions.trim());
    s = fill(&s, "id", args.id);
    s = fill(&s, "subject", args.subject);
    s = fill(&s, "commentary", args.commentary);
    if !args.farce {
        s = fill(&s, "cite", args.cite);
    }
    if args.needs_tools && !args.farce {
        s = fill(&s, "pack", args.pack);
    }
    s
}

/// Tweet `/rework` refresh (empty instructions) user turn.
#[must_use]
pub fn tweet_rework_refresh_user_message(
    id: &str,
    subject: &str,
    pack: &str,
    commentary: &str,
    cite: &str,
    ban_block: &str,
) -> String {
    let mut s = fill(TWEET_REWORK_REFRESH_USER_TMPL, "id", id);
    s = fill(&s, "subject", subject);
    s = fill(&s, "pack", pack);
    s = fill(&s, "commentary", commentary);
    s = fill(&s, "cite", cite);
    fill(&s, "ban_block", ban_block)
}

/// Commentary placeholder when the stored tweet body was an essay dump.
#[must_use]
pub fn tweet_rework_commentary_exploded() -> &'static str {
    TWEET_REWORK_COMMENTARY_EXPLODED.trim()
}

/// Commentary placeholder when the stored tweet body is empty.
#[must_use]
pub fn tweet_rework_commentary_empty() -> &'static str {
    TWEET_REWORK_COMMENTARY_EMPTY.trim()
}

/// `{pack_note}` for the tweet writer user turn.
/// `subject_https`: operator subject/instructions already contain an https URL to reuse.
#[must_use]
pub fn tweet_pack_note(pack_urls_empty: bool, subject_https: bool) -> &'static str {
    if subject_https {
        TWEET_PACK_NOTE_SUBJECT_HTTPS.trim()
    } else if pack_urls_empty {
        TWEET_PACK_NOTE_EMPTY.trim()
    } else {
        TWEET_PACK_NOTE_NORMAL.trim()
    }
}

/// `{pack_note}` for the draft writer user turn.
#[must_use]
pub fn draft_pack_note(pack_urls_empty: bool, brief_has_cite: bool) -> &'static str {
    if brief_has_cite {
        DRAFT_PACK_NOTE_SUBJECT_HTTPS.trim()
    } else if pack_urls_empty {
        DRAFT_PACK_NOTE_EMPTY.trim()
    } else {
        DRAFT_PACK_NOTE_NORMAL.trim()
    }
}

#[cfg(all(test, itcy_kitchen_prompts))]
mod tests {
    use super::*;

    #[test]
    fn identity_and_writer_cores() {
        assert!(WHO_IS_WHO.contains("ITCy"));
        assert!(WHO_IS_WHO.contains("@Interchouette") || WHO_IS_WHO.contains("X account"));
        assert!(WHO_IS_WHO.to_ascii_lowercase().contains("linkedin"));
        assert!(WHO_IS_WHO.contains("Match the surface"));
        assert!(WHO_IS_WHO.contains('🦉') && WHO_IS_WHO.contains('🦀'));
        assert!(WHO_IS_WHO.contains("owl") && WHO_IS_WHO.contains("Form craft"));
        assert!(!WHO_IS_WHO.ends_with("\n\n\n"));
        assert!(LOAD_SYSTEM_CORE.contains("ResearchPack"));
        assert!(DRAFT_SYSTEM_CORE.contains("SUBJECT LOCK"));
        assert!(DRAFT_SYSTEM_CORE.to_ascii_lowercase().contains("x account"));
        assert!(
            DRAFT_SYSTEM_CORE.contains("that URL line is required")
                || DRAFT_SYSTEM_CORE.contains("Do not omit a pack URL")
        );
        assert!(
            DRAFT_SYSTEM_CORE.contains("not an X tweet")
                || DRAFT_SYSTEM_CORE.contains("Forbidden: X-style")
        );
        assert!(DRAFT_SYSTEM_CORE.contains("Form craft"));
        assert!(DRAFT_SYSTEM_CORE.contains("180-280") || DRAFT_USER_TMPL.contains("180-280"));
        assert!(!DRAFT_SYSTEM_CORE.contains("120-220"));
        assert!(!DRAFT_USER_TMPL.contains("120-220"));
        assert!(DRAFT_USER_TMPL.contains('🦉') && DRAFT_USER_TMPL.contains('🦀'));
        assert!(
            DRAFT_SYSTEM_CORE.contains("/propose_draft")
                || DRAFT_USER_TMPL.contains("propose_draft")
        );
        assert!(
            DRAFT_SYSTEM_CORE.contains("2 or 3 unique")
                || DRAFT_USER_TMPL.contains("2 or 3 unique")
                || DRAFT_SYSTEM_CORE.contains("context-first")
                || DRAFT_USER_TMPL.contains("Context glyph first")
                || DRAFT_SYSTEM_CORE.contains("emoji compulsory")
        );
        assert!(TWEET_SYSTEM_CORE.to_ascii_lowercase().contains("x account"));
        assert!(TWEET_SYSTEM_CORE.contains("X SURFACE"));
        assert!(TWEET_SYSTEM_CORE.to_ascii_lowercase().contains("override"));
        assert!(TWEET_REWORK_SYSTEM_CORE
            .to_ascii_lowercase()
            .contains("copy-edit"));
        assert!(TWEET_SYSTEM_CORE.contains("280"));
        assert!(TWEET_SYSTEM_CORE
            .to_ascii_lowercase()
            .contains("blank line"));
        assert!(
            TWEET_SYSTEM_CORE.contains("publisher page or X status")
                || TWEET_SYSTEM_CORE.to_ascii_lowercase().contains("quote")
                || TWEET_SYSTEM_CORE
                    .to_ascii_lowercase()
                    .contains("quote card")
        );
        assert!(TWEET_SYSTEM_CORE.contains("Form craft"));
        assert!(TWEET_USER_TMPL.contains('🦉') && TWEET_USER_TMPL.contains('🦀'));
        assert!(
            TWEET_SYSTEM_CORE.contains("2 or 3 unique")
                || TWEET_USER_TMPL.contains("2 or 3 unique")
                || TWEET_SYSTEM_CORE.contains("Context glyph first")
                || TWEET_USER_TMPL.contains("Context glyph first")
                || TWEET_SYSTEM_CORE.contains("emoji compulsory")
        );
        assert!(
            TWEET_SYSTEM_CORE.contains("/propose_tweet")
                || TWEET_USER_TMPL.contains("propose_tweet")
        );
        assert!(
            !TWEET_USER_TMPL.contains("Call corpus_search once"),
            "tweet writer must not mandate corpus_search"
        );
        assert!(
            TWEET_SYSTEM_CORE.contains("Do **not** call `web_search`")
                || TWEET_SYSTEM_CORE
                    .to_ascii_lowercase()
                    .contains("writer tools")
                || TWEET_SYSTEM_CORE.contains("none in this turn"),
            "tweet writer must not invite second SERP"
        );
        assert!(TWEET_FARCE_SYSTEM_CORE.contains("@grok"));
        assert!(TWEET_FARCE_SYSTEM_CORE.contains("@cursor_ai"));
        assert!(TWEET_FARCE_SYSTEM_CORE.contains("@elonmusk"));
        assert!(TWEET_FARCE_SYSTEM_CORE
            .to_ascii_lowercase()
            .contains("farce"));
        assert!(FREEFORM_SYSTEM_CORE.contains("runtime only"));
        assert!(AI_CMO.contains("AI CMO"));
        assert!(AI_CMO.contains('🦉') && AI_CMO.contains('🦀'));
        assert!(AI_CMO.contains("/propose_draft") && AI_CMO.contains("/propose_tweet"));
        assert!(AI_CMO.contains("EXECUTION CHECKLIST"));
        assert!(AI_CMO.len() > 10000, "AI CMO framing should be substantial");
    }

    #[test]
    fn craft_prompts_forbid_markdown_bold_spans() {
        assert!(
            FORM_CRAFT_X.contains("Emphasis: plain prose"),
            "X Form craft must teach rare emphasis (no markdown bold flood)"
        );
        assert!(
            FORM_CRAFT_LINKEDIN.contains("Emphasis: plain prose"),
            "LinkedIn Form craft must teach rare emphasis"
        );
        for (name, body) in [
            ("FORM_CRAFT_X", FORM_CRAFT_X),
            ("FORM_CRAFT_LINKEDIN", FORM_CRAFT_LINKEDIN),
            ("CREATIVE_X", CREATIVE_X),
            ("CREATIVE_LINKEDIN", CREATIVE_LINKEDIN),
            ("AI_CMO", AI_CMO),
            ("TWEET_SYSTEM_CORE", TWEET_SYSTEM_CORE),
            ("DRAFT_SYSTEM_CORE", DRAFT_SYSTEM_CORE),
        ] {
            assert!(
                !body.contains("**"),
                "{name} must not teach markdown bold with ** spans (model copies them into artifacts)"
            );
        }
    }

    #[test]
    #[cfg(itcy_kitchen_prompts)]
    fn craft_prompts_forbid_watching_habit_stock_sticker() {
        // Voice bank used to teach this exact cloneable line; model shipped it as openers.
        assert!(
            !AI_CMO.contains("Curious owl: \"🦉 is watching how the rule becomes habit.\""),
            "AI_CMO must not teach the stock owl habit sticker as a voice-bank example"
        );
        for (name, body) in [
            ("AI_CMO", AI_CMO),
            ("DRAFT_SYSTEM_CORE", DRAFT_SYSTEM_CORE),
            ("FORM_CRAFT_LINKEDIN", FORM_CRAFT_LINKEDIN),
            ("CREATIVE_LINKEDIN", CREATIVE_LINKEDIN),
        ] {
            let lower = body.to_ascii_lowercase();
            assert!(
                lower.contains("becomes habit")
                    && (lower.contains("forbidden") || lower.contains("never")),
                "{name} must forbid watching-how/becomes-habit stock stickers"
            );
        }
    }

    #[test]
    fn surface_form_and_creative_craft() {
        assert!(FORM_CRAFT_X.contains('🦉') && FORM_CRAFT_X.contains('🦀'));
        assert!(FORM_CRAFT_LINKEDIN.contains('🦉') && FORM_CRAFT_LINKEDIN.contains('🦀'));
        assert!(
            FORM_CRAFT_X.contains("EMIT RULE")
                || FORM_CRAFT_X.contains("never write `(ITCy)`")
                || FORM_CRAFT_X.contains("Never `(ITCy)`")
                || FORM_CRAFT_X.contains("`(ITCy)`")
        );
        assert!(!FORM_CRAFT_X.contains("🦉 (ITCy)"));
        assert!(!FORM_CRAFT_X.contains("🦀 (Rust)"));
        assert!(!FORM_CRAFT_LINKEDIN.contains("🦉 (ITCy)"));
        assert!(!FORM_CRAFT_LINKEDIN.contains("🦀 (Rust)"));
        assert!(FORM_CRAFT_X.contains("Form craft"));
        assert!(FORM_CRAFT_LINKEDIN.contains("Form craft"));
        assert!(FORM_CRAFT_X.contains("EMOJI AS LANGUAGE"));
        assert!(FORM_CRAFT_LINKEDIN.contains("EMOJI AS LANGUAGE"));
        assert!(FORM_CRAFT_X.contains("compulsory"));
        assert!(FORM_CRAFT_LINKEDIN.contains("compulsory"));
        assert!(
            FORM_CRAFT_X.contains("2 or 3 unique")
                || FORM_CRAFT_X.contains("1 to 3 unique")
                || FORM_CRAFT_X.contains("1-3 unique")
        );
        assert!(
            FORM_CRAFT_LINKEDIN.contains("2 or 3 unique")
                || FORM_CRAFT_LINKEDIN.contains("1 to 3 unique")
                || FORM_CRAFT_LINKEDIN.contains("1-3 unique")
        );
        assert!(
            FORM_CRAFT_LINKEDIN.contains("Context glyph")
                || FORM_CRAFT_LINKEDIN.contains("context glyph")
                || FORM_CRAFT_LINKEDIN.contains("context-first")
        );
        assert!(
            FORM_CRAFT_X.contains("context glyph")
                || FORM_CRAFT_X.contains("Context first")
                || FORM_CRAFT_X.contains("context-first")
        );
        assert!(
            FORM_CRAFT_LINKEDIN.contains("1 alone is weak")
                || FORM_CRAFT_X.contains("1 alone is weak")
        );
        assert!(FORM_CRAFT_X.contains("motifs") || FORM_CRAFT_X.contains("Context palette"));
        assert!(
            FORM_CRAFT_X.contains("OPERATOR SCOPE")
                || FORM_CRAFT_X.contains("Operator instructions win")
        );
        assert!(FORM_CRAFT_X.contains("hashtag line"));
        assert!(FORM_CRAFT_X.contains("blank line between beats"));
        assert!(!FORM_CRAFT_LINKEDIN.contains("blank line between beats"));
        assert!(FORM_CRAFT_X.contains("clear stance") && FORM_CRAFT_X.contains("clear signal"));
        assert!(
            FORM_CRAFT_LINKEDIN.contains("Forbidden on LinkedIn")
                || FORM_CRAFT_LINKEDIN.contains("aerated")
                || FORM_CRAFT_LINKEDIN.contains("X-style")
        );
        assert!(!FORM_CRAFT_LINKEDIN.contains("Target **280**"));
        assert!(
            FORM_CRAFT_X.contains("280")
                && (FORM_CRAFT_X.contains("50%")
                    || FORM_CRAFT_X.contains("~160-220")
                    || FORM_CRAFT_X.contains("root + reply")),
            "Form craft - X should teach length mix / ship reply, not only a hard 280 target"
        );
        assert!(
            FORM_CRAFT_X.len() > 10000,
            "Form craft - X should be a rich curriculum"
        );
        assert!(
            FORM_CRAFT_LINKEDIN.len() > 4000,
            "Form craft - LinkedIn should be a real curriculum (jobs + bad-only, no slot skeletons)"
        );
    }

    #[test]
    fn creative_craft_surface_locks() {
        assert!(CREATIVE_X.contains("Creative CMO") || CREATIVE_X.contains("CREATIVE MANDATE"));
        assert!(
            CREATIVE_LINKEDIN.contains("Creative CMO")
                || CREATIVE_LINKEDIN.contains("CREATIVE MANDATE")
        );
        assert!(CREATIVE_X.contains('🦉') && CREATIVE_X.contains('🦀'));
        assert!(CREATIVE_LINKEDIN.contains('🦉') && CREATIVE_LINKEDIN.contains('🦀'));
        assert!(!CREATIVE_X.contains("🦉 (ITCy)"));
        assert!(!CREATIVE_LINKEDIN.contains("🦉 (ITCy)"));
        assert!(
            CREATIVE_X.len() > 8000,
            "Creative CMO - X studio should be substantial"
        );
        assert!(
            CREATIVE_LINKEDIN.len() > 4000,
            "Creative CMO - LinkedIn studio should be a real curriculum (jobs + bad-only)"
        );
        // Slot skeletons teach small models to emit mad-libs - keep them out of all craft prompts.
        assert!(!CREATIVE_X.contains("[ENTITY]"));
        assert!(!CREATIVE_X.contains("[CONTEXT_EMOJI]"));
        assert!(!CREATIVE_LINKEDIN.contains("[ENTITY]"));
        assert!(!FORM_CRAFT_LINKEDIN.contains("[ENTITY]"));
    }

    #[test]
    fn linkedin_craft_locks_owl_emoji_baseline() {
        // LinkedIn identity: owl is baseline voice, not optional "when earned".
        assert!(
            !FORM_CRAFT_LINKEDIN.contains("only when earned"),
            "LinkedIn Form craft must not teach owl/crab as optional 'when earned'"
        );
        assert!(
            !CREATIVE_LINKEDIN.contains("only when earned"),
            "LinkedIn Creative must not teach owl/crab as optional 'when earned'"
        );
        assert!(
            !DRAFT_SYSTEM_CORE.contains("only when earned")
                && !DRAFT_USER_TMPL.contains("only when earned"),
            "draft writer prompts must not teach owl/crab as optional 'when earned'"
        );
        assert!(
            FORM_CRAFT_LINKEDIN.contains("Zero emoji is a fail")
                || FORM_CRAFT_LINKEDIN.contains("you are the owl"),
            "LinkedIn Form craft must lock zero-emoji fail / owl identity"
        );
        assert!(
            CREATIVE_LINKEDIN.contains("Zero emoji is a fail")
                || CREATIVE_LINKEDIN.contains("you are the owl")
                || CREATIVE_LINKEDIN.contains("owl AI CMO"),
            "LinkedIn Creative must lock owl AI CMO + emoji fail"
        );
        assert!(
            DRAFT_SYSTEM_CORE.contains("zero emoji is a fail")
                || DRAFT_USER_TMPL.contains("Zero emoji is a fail"),
            "draft writer must say zero emoji fails"
        );
    }

    #[test]
    fn templates_substitute_placeholders() {
        let u = load_user_message("rust async");
        assert!(u.contains("rust async"));
        assert!(!u.contains("{subject}"));
        let d = draft_user_message("PACK", "NOTE", "subj");
        assert!(d.contains("PACK") && d.contains("NOTE") && d.contains("subj"));
        assert!(!d.contains("{research_pack}"));
        let t = tweet_user_message("PACK", "NOTE", "subj");
        assert!(t.contains("PACK") && t.contains("NOTE") && t.contains("subj"));
        assert!(!t.contains("{research_pack}"));
        let farce = tweet_farce_user_message("Mars Wi-Fi");
        assert!(farce.contains("Mars Wi-Fi"));
        assert!(!farce.contains("{theme}"));
        let dr =
            draft_rework_user_message("shorter", "DRAFT-1", "subj", "PACK", "body", "https://a");
        assert!(dr.contains("shorter") && dr.contains("DRAFT-1") && dr.contains("PACK"));
        assert!(dr.contains("SCOPE:"));
        assert!(
            dr.contains("Form craft") || dr.contains("emoji as language") || dr.contains("1-3")
        );
        assert!(!dr.contains("{instructions}"));
        let tr = tweet_rework_user_message(&TweetReworkUserArgs {
            instructions: "add emojis",
            id: "TWEET-1",
            subject: "rust",
            commentary: "hi",
            cite: "https://a",
            pack: "PACK",
            farce: false,
            needs_tools: false,
        });
        assert!(tr.contains("SCOPE:"));
        assert!(
            tr.contains("Form craft")
                || tr.contains("1-3")
                || tr.contains("emoji compulsory")
                || tr.contains("Emoji compulsory")
        );
        assert!(tr.contains("keep / restore prose") || tr.contains("only emoji"));
        assert!(
            tr.contains("NOT a fact lock") || tr.contains("highest priority"),
            "rework must not freeze the digest subject as facts: {tr}"
        );
        assert!(
            !tr.contains("Subject (locked)"),
            "Subject (locked) fights operator corrections: {tr}"
        );
        assert!(!tr.contains("{commentary}"));
        assert!(TWEET_REWORK_SYSTEM_CORE.contains("OPERATOR OVERRIDE"));
        assert!(!TWEET_REWORK_SYSTEM_CORE.contains("SUBJECT LOCK"));
        assert!(TWEET_REWORK_SYSTEM_CORE.contains("fact lock"));
        let tr_tools = tweet_rework_user_message(&TweetReworkUserArgs {
            instructions: "browse the repo",
            id: "TWEET-1",
            subject: "rust",
            commentary: "hi",
            cite: "https://a",
            pack: "PACK-BODY",
            farce: false,
            needs_tools: true,
        });
        assert!(tr_tools.contains("PACK-BODY"));
        assert!(tr_tools.contains("ResearchPack"));
        let tr_farce = tweet_rework_user_message(&TweetReworkUserArgs {
            instructions: "funnier",
            id: "TWEET-1",
            subject: "joke",
            commentary: "hi",
            cite: "(none)",
            pack: "",
            farce: true,
            needs_tools: false,
        });
        assert!(tr_farce.contains("FARCE"));
        assert!(tr_farce.contains("No https"));
        assert!(!tr_farce.contains("{cite}"));
    }

    #[test]
    fn draft_explains_corpus_is_voice_not_browse() {
        let lower = DRAFT_SYSTEM_CORE.to_ascii_lowercase();
        assert!(lower.contains("voice"));
        assert!(lower.contains("linkedin"));
        assert!(lower.contains("browse_url") || lower.contains("browse"));
    }

    #[test]
    fn x_craft_bans_not_just_broader_trend_mush() {
        assert!(
            CREATIVE_X.contains("it's not about") || CREATIVE_X.contains("not just another tool"),
            "Creative X must ban not-just / not-about slogan class"
        );
        assert!(
            CREATIVE_X.contains("broader trend"),
            "Creative X must ban broader-trend mush"
        );
        assert!(
            FORM_CRAFT_X.contains("not just")
                || FORM_CRAFT_X.contains("not-about")
                || FORM_CRAFT_X.contains("broader trend"),
            "Form craft X must forbid not-just mush"
        );
        let tweet_user = TWEET_USER_TMPL;
        assert!(
            tweet_user.contains("Slogan mush") || tweet_user.contains("broader trend"),
            "tweet user must forbid slogan mush"
        );
    }

    #[test]
    fn linkedin_craft_bans_not_just_broader_trend_mush() {
        assert!(
            CREATIVE_LINKEDIN.contains("it's not about")
                || CREATIVE_LINKEDIN.contains("not just another tool"),
            "Creative LinkedIn must ban not-just / not-about slogan class"
        );
        assert!(
            CREATIVE_LINKEDIN.contains("broader trend"),
            "Creative LinkedIn must ban broader-trend mush"
        );
        assert!(
            FORM_CRAFT_LINKEDIN.contains("not-just / broader-trend")
                || FORM_CRAFT_LINKEDIN.contains("It's not just another tool"),
            "Form craft LinkedIn must show not-just mush as a Bad contrast"
        );
        let cite_user = DRAFT_USER_SUBJECT_HTTPS_TMPL;
        assert!(
            cite_user.contains("Slogan mush") || cite_user.contains("broader trend"),
            "cite-locked draft user must forbid slogan mush"
        );
        assert!(
            DRAFT_PACK_NOTE_SUBJECT_HTTPS.contains("primary")
                && DRAFT_PACK_NOTE_SUBJECT_HTTPS.contains("secondary"),
            "cite pack note must state cite-primary / SERP-secondary hierarchy"
        );
    }

    #[test]
    fn draft_pack_note_subject_https_forbids_corpus() {
        let note = draft_pack_note(false, true).to_ascii_lowercase();
        assert!(note.contains("corpus_search"));
        assert!(note.contains("do not"));
        assert!(note.contains("linkedin"));
    }

    #[test]
    fn draft_user_cite_path_omits_corpus_search_instruction() {
        let user =
            draft_user_message_subject_https("PACK", draft_pack_note(false, true), "subject");
        let lower = user.to_ascii_lowercase();
        assert!(!lower.contains("call corpus_search"));
        assert!(lower.contains("corpus_search"));
        assert!(lower.contains("write from researchpack"));
    }

    #[test]
    fn draft_user_normal_path_keeps_corpus_search_instruction() {
        let user = draft_user_message("PACK", draft_pack_note(false, false), "subject");
        assert!(user.to_ascii_lowercase().contains("call corpus_search"));
    }
}

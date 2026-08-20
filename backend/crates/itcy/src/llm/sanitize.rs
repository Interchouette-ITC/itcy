// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Sanitize model / draft text for `ITCy` hard rules (no em dash, etc.).

/// Unicode em dash (U+2014) - forbidden in `ITCy` artifacts.
pub const EM_DASH: char = '\u{2014}';

/// Unicode en dash (U+2013). Digit ranges become `2024-2026`; pause uses become a comma.
pub const EN_DASH: char = '\u{2013}';

/// Strips / replaces characters forbidden in public and Slack `ITCy` text.
///
/// Em dash (U+2014), including ` — ` (spaces around it), becomes `, `. Never
/// rewrite an em dash into an ASCII hyphen. Spaced hyphen pauses (` - `, or any
/// Unicode whitespace around `-`) also become `, ` (models cheat past em-dash
/// bans this way). Digit en-dash ranges stay `2024-2026`. Compound hyphens
/// (`memory-safety`) are unchanged. Slack `:shortcode:` becomes Unicode (`X`
/// and `LinkedIn` cannot render colon codes).
#[must_use]
pub fn sanitize_itcy_text(input: &str) -> String {
    let mut s = tight_hyphen_digit_en_dashes(input);
    s = s.replace(&format!(" {EM_DASH} "), ", ");
    s = s.replace(EM_DASH, ", ");
    s = s.replace(&format!(" {EN_DASH} "), ", ");
    s = s.replace(EN_DASH, ", ");
    s = replace_spaced_hyphen_pauses(&s);
    collapse_spaces(&mut s);
    tidy_commas(&mut s);
    s = expand_emoji_shortcodes(&s);
    collapse_spaces(&mut s);
    s
}

/// `word - word` / NBSP-hyphen-NBSP → `word, word`. Leaves `memory-safety` alone.
fn replace_spaced_hyphen_pauses(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < n {
        if chars[i] == '-'
            && i > 0
            && i + 1 < n
            && chars[i - 1].is_whitespace()
            && chars[i + 1].is_whitespace()
        {
            while out.ends_with(char::is_whitespace) {
                out.pop();
            }
            out.push_str(", ");
            i += 1;
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `2024–2026` → `2024-2026` (range hyphen, not a pause).
fn tight_hyphen_digit_en_dashes(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < n {
        if chars[i] == EN_DASH
            && i > 0
            && i + 1 < n
            && chars[i - 1].is_ascii_digit()
            && chars[i + 1].is_ascii_digit()
        {
            out.push('-');
            i += 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn tidy_commas(s: &mut String) {
    while s.contains(" ,") {
        *s = s.replace(" ,", ",");
    }
    while s.contains(",,") {
        *s = s.replace(",,", ",");
    }
}

/// Slack / GitHub `:shortcode:` → Unicode. Unknown colon codes are dropped.
#[must_use]
pub fn expand_emoji_shortcodes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' && shortcode_can_open(input, i) {
            if let Some((name, consumed)) = take_shortcode(&input[i..]) {
                if let Some(emoji) = shortcode_to_emoji(name) {
                    out.push_str(emoji);
                    i += consumed;
                    continue;
                }
                // Looks like `:name:` but is not a known emoji: never ship it to X / LinkedIn.
                i += consumed;
                continue;
            }
        }
        let Some(ch) = input[i..].chars().next() else {
            break;
        };
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// True when operator instructions ask to add / keep emoji.
#[must_use]
pub fn instructions_ask_for_emoji(instructions: &str) -> bool {
    let t = instructions.to_ascii_lowercase();
    t.contains("emoji")
        || t.contains("emojis")
        || t.contains("smiley")
        || t.contains("😀")
        || t.contains(":owl:")
        || t.contains(":rocket:")
        || t.contains(":sparkles:")
}

/// True when `text` already has at least one Unicode emoji glyph (after shortcode expand).
#[must_use]
pub fn text_contains_emoji(text: &str) -> bool {
    count_emoji(text) > 0
}

/// Count emoji glyphs in `text` (colon forms like `:owl:` count after expand).
#[must_use]
pub fn count_emoji(text: &str) -> usize {
    expand_emoji_shortcodes(text)
        .chars()
        .filter(|c| char_is_emoji_like(*c))
        .count()
}

/// Tweet craft bar: at least two emoji in the body (prompts ask for 2-4).
#[must_use]
pub fn tweet_emoji_ok(text: &str) -> bool {
    count_emoji(text) >= 2
}

fn char_is_emoji_like(c: char) -> bool {
    let u = c as u32;
    matches!(
        u,
        0x231A..=0x231B
            | 0x23E9..=0x23F3
            | 0x23F8..=0x23FA
            | 0x25AA..=0x25AB
            | 0x25B6
            | 0x25C0
            | 0x25FB..=0x25FE
            | 0x2600..=0x27BF
            | 0x2934..=0x2935
            | 0x2B05..=0x2B07
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
            | 0x1F000..=0x1FAFF
    ) || (0x1F1E6..=0x1F1FF).contains(&u)
}

fn collapse_spaces(s: &mut String) {
    while s.contains("  ") {
        *s = s.replace("  ", " ");
    }
}

fn shortcode_can_open(input: &str, colon_at: usize) -> bool {
    if colon_at == 0 {
        return true;
    }
    input[..colon_at]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_ascii_alphanumeric())
}

fn is_shortcode_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'+' | b'-')
        })
}

fn take_shortcode(rest: &str) -> Option<(&str, usize)> {
    let after = rest.strip_prefix(':')?;
    let end = after.find(':')?;
    let name = &after[..end];
    if !is_shortcode_name(name) {
        return None;
    }
    Some((name, name.len() + 2))
}

fn shortcode_to_emoji(name: &str) -> Option<&'static str> {
    // Slack names win when they disagree with gemoji (`:feet:` is footprints, not paws).
    if let Some(alias) = slack_alias_to_gemoji(name) {
        return emojis::get_by_shortcode(alias).map(emojis::Emoji::as_str);
    }
    emojis::get_by_shortcode(name).map(emojis::Emoji::as_str)
}

/// Slack short names that differ from gemoji.
fn slack_alias_to_gemoji(name: &str) -> Option<&'static str> {
    Some(match name {
        "feet" => "footprints",
        "thumbsup" => "+1",
        "thumbsdown" => "-1",
        "simple_smile" => "slightly_smiling_face",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kills_em_dash() {
        let s = sanitize_itcy_text("growth\u{2014}it’s quantum");
        assert!(!s.contains(EM_DASH));
        assert!(s.contains("growth, it’s quantum"));
        assert!(!s.contains('-'), "em dash must not become a hyphen: {s}");
    }

    #[test]
    fn space_em_dash_space_becomes_comma_space() {
        let s = sanitize_itcy_text("landscape \u{2014} one that speaks");
        assert_eq!(s, "landscape, one that speaks");
        assert!(!s.contains(EM_DASH));
        assert!(!s.contains(" - "));
    }

    #[test]
    fn kills_en_dash() {
        let s = sanitize_itcy_text("2024–2026");
        assert!(!s.contains(EN_DASH));
        assert_eq!(s, "2024-2026");
    }

    #[test]
    fn spaced_hyphen_pause_becomes_comma() {
        let s = sanitize_itcy_text(
            "This isn’t a sign of complacency - it’s a reflection of Rust’s design.",
        );
        assert!(s.contains("complacency, it’s a reflection"));
        assert!(!s.contains(" - "));
        let compound = sanitize_itcy_text("memory-safety and C/C++ stay hyphenated.");
        assert!(compound.contains("memory-safety"));
        assert!(compound.contains("C/C++"));
    }

    #[test]
    fn nbsp_spaced_hyphen_pause_becomes_comma() {
        let s = sanitize_itcy_text("code\u{00a0}-\u{00a0}it’s about trust");
        assert_eq!(s, "code, it’s about trust");
    }

    #[test]
    fn expands_greg_owl_sticker_line() {
        let s = sanitize_itcy_text(
            ":owl: is watching how this shift becomes habit. :crab: energy: careful diffs.",
        );
        assert!(s.contains('🦉'));
        assert!(s.contains('🦀'));
        assert!(!s.contains(":owl:"));
        assert!(!s.contains(":crab:"));
    }

    #[test]
    fn expands_greg_tweet_shortcodes() {
        let s = expand_emoji_shortcodes(
            "Hello! :feet: I’m ITCy, a Linux owl who loves IT, humour, and wordplay. I’m the AI CMO running Interchouette ITC’s X account. Rust, TDD, and open experiments are my focus. Let’s build something fun. :owl::computer:",
        );
        assert!(!s.contains(':'));
        assert!(s.contains('🦉'));
        assert!(s.contains('💻'));
        assert!(s.contains('👣'));
        assert!(s.contains("Hello!"));
    }

    #[test]
    fn expands_gemoji_catalog_not_a_hand_list() {
        let s = expand_emoji_shortcodes(":rocket: :sparkles: :penguin: :100:");
        assert!(s.contains('🚀'));
        assert!(s.contains('✨'));
        assert!(s.contains('🐧'));
        assert!(s.contains('💯'));
        assert!(!s.contains(':'));
    }

    #[test]
    fn sanitize_keeps_blank_lines() {
        let s = sanitize_itcy_text("Hello!\n\nI’m ITCy.");
        assert!(s.contains("Hello!\n\nI’m ITCy."));
    }

    #[test]
    fn sanitize_also_expands_slack_colon_emoji() {
        let s = sanitize_itcy_text("Hello :owl:");
        assert!(s.contains('🦉'));
        assert!(!s.contains(":owl:"));
    }

    #[test]
    fn leaves_times_alone() {
        assert_eq!(expand_emoji_shortcodes("12:25 PM"), "12:25 PM");
    }

    #[test]
    fn drops_unknown_colon_codes() {
        assert_eq!(
            sanitize_itcy_text("Hello :not_an_emoji: world"),
            "Hello world"
        );
    }

    #[test]
    fn keeps_clock_times_and_ports() {
        assert_eq!(expand_emoji_shortcodes("12:25:00"), "12:25:00");
        assert_eq!(
            expand_emoji_shortcodes("https://example.com:443/a"),
            "https://example.com:443/a"
        );
    }

    #[test]
    fn expands_plus_one_and_hundred() {
        let s = expand_emoji_shortcodes(":+1: :100:");
        assert!(s.contains('👍'));
        assert!(s.contains('💯'));
        assert!(!s.contains(':'));
    }

    #[test]
    fn slack_feet_is_footprints_not_paws() {
        let s = expand_emoji_shortcodes(":feet:");
        assert!(s.contains('👣'));
        assert!(!s.contains('🐾'));
    }

    #[test]
    fn unicode_emoji_passthrough() {
        let s = sanitize_itcy_text("Hello 🦉💻");
        assert!(s.contains('🦉'));
        assert!(s.contains('💻'));
    }

    #[test]
    fn detects_emoji_ask_and_presence() {
        assert!(instructions_ask_for_emoji("add emojis !!!!"));
        assert!(instructions_ask_for_emoji("more :owl: please"));
        assert!(!instructions_ask_for_emoji("make it shorter"));
        assert!(text_contains_emoji("Rust policy 🦀 on AI"));
        assert!(text_contains_emoji("Hello :owl:"));
        assert!(!text_contains_emoji("No glyphs here #Rust"));
        assert!(tweet_emoji_ok("🦉 Rust 🦀 policy"));
        assert!(!tweet_emoji_ok("only one ✨ mark"));
        assert_eq!(count_emoji("🦉 🦀 ✨"), 3);
    }

    #[test]
    fn expands_every_gemoji_shortcode() {
        let mut n = 0_u32;
        for emoji in emojis::iter() {
            for code in emoji.shortcodes() {
                if !is_shortcode_name(code) || slack_alias_to_gemoji(code).is_some() {
                    continue;
                }
                let input = format!("go :{code}: stop");
                let out = expand_emoji_shortcodes(&input);
                assert!(
                    !out.contains(&format!(":{code}:")),
                    "shortcode left in place: {code}"
                );
                assert!(
                    out.contains(emoji.as_str()),
                    "missing glyph for :{code}: (want {})",
                    emoji.as_str()
                );
                n += 1;
            }
        }
        assert!(
            n > 1500,
            "gemoji catalog too small to prove coverage (got {n})"
        );
    }
}

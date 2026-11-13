// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Publication prose boundaries. Byte offsets always refer to the unchanged input.

use unicode_segmentation::UnicodeSegmentation;

pub(crate) fn is_emoji(glyph: &str) -> bool {
    emojis::get(glyph).is_some() || emojis::get(&glyph.replace('\u{fe0f}', "")).is_some()
}

fn closes_sentence(text: &str) -> bool {
    text.trim_end()
        .trim_end_matches(['"', '\'', '\u{2019}', '\u{201d}', ')', ']'])
        .ends_with(['.', '?', '!', ':', ';'])
}

fn token_start(text: &str) -> usize {
    text.char_indices()
        .rfind(|(_, ch)| ch.is_whitespace())
        .map_or(0, |(offset, ch)| offset + ch.len_utf8())
}

/// Normalize unambiguous closing emoji, never leading or inline emoji.
#[must_use]
pub fn normalize_publication_prose(text: &str) -> String {
    let text = crate::llm::sanitize::strip_spurious_period_after_emoji(text).replace("\r\n", "\n");
    text.split("\n\n")
        .map(normalize_paragraph)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn normalize_paragraph(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with("http")
        || trimmed.starts_with('#')
        || trimmed.starts_with("Written by AI")
        || trimmed.starts_with("Link:")
    {
        return text.to_string();
    }
    let glyphs: Vec<(usize, &str)> = text.grapheme_indices(true).collect();
    let mut out = String::new();
    let mut cursor = 0;
    let mut i = 0;
    let mut in_code = false;
    while i < glyphs.len() {
        let (start, glyph) = glyphs[i];
        if glyph == "`" {
            in_code = !in_code;
        }
        let url_token = text[token_start(&text[..start])..start].contains("://");
        let line_opener = text[..start]
            .rsplit_once('\n')
            .is_some_and(|(_, tail)| tail.trim().is_empty());
        if in_code || url_token || line_opener || !is_emoji(glyph) {
            i += 1;
            continue;
        }
        let mut end = start + glyph.len();
        let mut j = i + 1;
        // A cluster may contain spaces between adjacent emoji, not trailing prose spaces.
        while j < glyphs.len() {
            if is_emoji(glyphs[j].1) {
                end = glyphs[j].0 + glyphs[j].1.len();
                j += 1;
            } else if glyphs[j].1 == " " && j + 1 < glyphs.len() && is_emoji(glyphs[j + 1].1) {
                j += 1;
            } else {
                break;
            }
        }
        let before = text[cursor..start].trim_end();
        let after = text[end..].trim_start();
        let moved = after
            .chars()
            .next()
            .filter(|c| matches!(c, '.' | '?' | '!'));
        let after_punct = moved.map_or(after, |c| after[c.len_utf8()..].trim_start());
        let closes = after_punct.is_empty()
            || (text[end..].starts_with(char::is_whitespace)
                && after_punct.starts_with(char::is_uppercase))
            || (closes_sentence(before) && after_punct.starts_with(char::is_uppercase))
            || moved.is_some();
        // An opener has no prose before it in this paragraph / newly emitted beat.
        if closes && before.chars().any(char::is_alphanumeric) {
            out.push_str(before);
            if !closes_sentence(before) {
                out.push(moved.unwrap_or('.'));
            }
            out.push(' ');
            out.push_str(&text[start..end]);
            cursor = end;
            if let Some(punct) = moved {
                cursor += text[end..].find(punct).expect("punctuation exists") + punct.len_utf8();
            }
            if !text[cursor..].trim().is_empty() {
                out.push_str("\n\n");
                cursor += text[cursor..].len() - text[cursor..].trim_start().len();
            }
        }
        i = j;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Sentence ends outside code/URLs/tokens, including trailing emoji and closing quotes.
pub(crate) fn sentence_ends(text: &str) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut in_code = false;
    for (i, ch) in text.char_indices() {
        if ch == '`' {
            in_code = !in_code;
        }
        if in_code || !matches!(ch, '.' | '!' | '?') {
            continue;
        }
        let token = &text[token_start(&text[..i])..=i];
        if token.contains("://")
            || matches!(
                token,
                "Mr." | "Mrs." | "Ms." | "Dr." | "Prof." | "e.g." | "i.e." | "vs."
            )
            || (token.len() == 2 && token.starts_with(char::is_uppercase))
        {
            continue;
        }
        let mut end = i + ch.len_utf8();
        while let Some(c) = text[end..].chars().next() {
            if matches!(c, '"' | '\'' | '\u{2019}' | '\u{201d}' | ')' | ']') {
                end += c.len_utf8();
            } else {
                break;
            }
        }
        if !text[end..].is_empty() && !text[end..].starts_with(char::is_whitespace) {
            continue;
        }
        let trailing_start = end;
        for (offset, glyph) in text[trailing_start..].grapheme_indices(true) {
            if glyph.contains('\n') {
                break;
            }
            if glyph.chars().all(char::is_whitespace) || is_emoji(glyph) {
                end = trailing_start + offset + glyph.len();
            } else {
                break;
            }
        }
        ends.push(end);
    }
    ends
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_emoji_are_punctuated_without_touching_inline_or_leading_glyphs() {
        for (raw, expected) in [
            ("Works 🚀.", "Works. 🚀"),
            ("Works? 🚀", "Works? 🚀"),
            ("Works 🚀 Next sentence.", "Works. 🚀\n\nNext sentence."),
            ("🤖 Builders use 🦀 to work.", "🤖 Builders use 🦀 to work."),
            (
                "He said \"works.\" 🚀 Next.",
                "He said \"works.\" 🚀\n\nNext.",
            ),
            ("Works 👩🏽‍💻🇫🇷 Next.", "Works. 👩🏽‍💻🇫🇷\n\nNext."),
            ("`a🚀.b` stays intact.", "`a🚀.b` stays intact."),
            (
                "Read https://rust-lang.org/🚀",
                "Read https://rust-lang.org/🚀",
            ),
            (
                "Hook.\n🔧 Tooling stays inline.",
                "Hook.\n🔧 Tooling stays inline.",
            ),
            ("Works. 🚀Next.", "Works. 🚀\n\nNext."),
            ("Worth a look: 📜", "Worth a look: 📜"),
            ("Works\u{a0}🚀", "Works. 🚀"),
            ("Works 🚀\r\n\r\nNext.", "Works. 🚀\n\nNext."),
        ] {
            assert_eq!(normalize_publication_prose(raw), expected);
            assert_eq!(normalize_publication_prose(expected), expected);
        }
    }

    #[test]
    fn sentence_boundaries_do_not_split_tokens() {
        let raw = "Dr. Jones uses `chat_template.jinja` on 3.13+ and 3.12. 🚀 Next.";
        let ends = sentence_ends(raw);
        assert_eq!(ends.len(), 2);
        assert_eq!(
            raw[..ends[0]].trim(),
            "Dr. Jones uses `chat_template.jinja` on 3.13+ and 3.12. 🚀"
        );
        assert_eq!(sentence_ends("Works\u{a0}well. Next.").len(), 2);
    }
}

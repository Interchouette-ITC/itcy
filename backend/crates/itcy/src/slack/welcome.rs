// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Welcome message when someone joins `#itcy`.

/// English welcome for `member_joined_channel` in the runtime channel.
#[must_use]
pub fn welcome_text(user_id: &str) -> String {
    format!(
        "Welcome <@{user_id}> to `#itcy`.\n\
         I am ITCy, the Interchouette ITC runtime bot.\n\
         Say `/help` for slash workflows, or `/status_itcy` for liveness.\n\
         Freeform chat is fine for questions; drafts/BAT use slash commands.\n\
         This channel is runtime only (status, alerts, operator loop). Contract edits stay in Cursor."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_mentions_user_and_help() {
        let text = welcome_text("U42");
        assert!(text.contains("<@U42>"));
        assert!(text.contains("help"));
        assert!(text.contains("runtime only"));
    }
}

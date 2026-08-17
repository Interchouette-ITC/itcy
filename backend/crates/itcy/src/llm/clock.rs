// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Shared clock helpers for LLM prompts (models need today's date).

use chrono::Local;

/// Calendar date for prompts, e.g. `Monday 2026-07-27` (host local).
#[must_use]
pub fn today_prompt_date() -> String {
    Local::now().format("%A %Y-%m-%d").to_string()
}

/// Short line to prepend into system prompts.
#[must_use]
pub fn today_context_line() -> String {
    format!(
        "Today's date (host local): {}. Prefer recent sources from this year ({}) when searching; \
do not assume outdated years like 2023 unless the operator asks for history of that year.",
        today_prompt_date(),
        Local::now().format("%Y")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn today_line_mentions_year() {
        let y = Local::now().format("%Y").to_string();
        let line = today_context_line();
        assert!(line.contains(&y));
        assert!(line.contains("Today's date"));
    }
}

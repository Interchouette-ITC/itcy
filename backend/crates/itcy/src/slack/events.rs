// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Parse Slack Events API payloads into runtime intents.

use serde::Deserialize;

/// High-level event the runtime cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedEvent {
    /// Human (or mention) message in a channel.
    Message {
        channel_id: String,
        user_id: String,
        text: String,
    },
    /// Slack slash command (Socket Mode `slash_commands`).
    SlashCommand {
        channel_id: String,
        user_id: String,
        command: String,
        text: String,
    },
    /// Someone joined the channel.
    MemberJoined { channel_id: String, user_id: String },
}

#[derive(Debug, Deserialize)]
pub struct SlackEventPayload {
    pub event: Option<SlackEventInner>,
}

#[derive(Debug, Deserialize)]
pub struct SlackEventInner {
    #[serde(rename = "type")]
    pub event_type: String,
    pub channel: Option<String>,
    pub user: Option<String>,
    pub text: Option<String>,
    pub bot_id: Option<String>,
    pub subtype: Option<String>,
}

const SKIP_MESSAGE_SUBTYPES: &[&str] = &[
    "bot_message",
    "channel_join",
    "channel_leave",
    "group_join",
    "group_leave",
];

/// Maps a Slack Events API payload to a parsed runtime event.
#[must_use]
pub fn events_payload_to_parsed(payload: SlackEventPayload) -> Option<ParsedEvent> {
    let event = payload.event?;
    event_inner_to_parsed(event)
}

fn event_inner_to_parsed(event: SlackEventInner) -> Option<ParsedEvent> {
    let channel_id = event.channel?;
    let user_id = event.user?;

    match event.event_type.as_str() {
        // `#itcy` is a dedicated runtime channel: every human `message` is enough.
        // Ignore `app_mention` - Slack also emits `message` for the same post, which
        // would double-reply if both were handled.
        "message" => {
            if event.bot_id.is_some() {
                return None;
            }
            if event
                .subtype
                .as_deref()
                .is_some_and(|s| SKIP_MESSAGE_SUBTYPES.contains(&s))
            {
                return None;
            }
            Some(ParsedEvent::Message {
                channel_id,
                user_id,
                text: event.text.unwrap_or_default(),
            })
        }
        "member_joined_channel" => Some(ParsedEvent::MemberJoined {
            channel_id,
            user_id,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_message() {
        let payload: SlackEventPayload = serde_json::from_value(json!({
            "event": {
                "type": "message",
                "channel": "C1",
                "user": "U1",
                "text": "hello"
            }
        }))
        .expect("parse");
        assert_eq!(
            events_payload_to_parsed(payload),
            Some(ParsedEvent::Message {
                channel_id: "C1".into(),
                user_id: "U1".into(),
                text: "hello".into(),
            })
        );
    }

    #[test]
    fn skips_bot_message() {
        let payload: SlackEventPayload = serde_json::from_value(json!({
            "event": {
                "type": "message",
                "channel": "C1",
                "user": "U1",
                "text": "hi",
                "bot_id": "B1"
            }
        }))
        .expect("parse");
        assert!(events_payload_to_parsed(payload).is_none());
    }

    #[test]
    fn ignores_app_mention_to_avoid_double_reply() {
        let payload: SlackEventPayload = serde_json::from_value(json!({
            "event": {
                "type": "app_mention",
                "channel": "C1",
                "user": "U1",
                "text": "<@BOT> hello"
            }
        }))
        .expect("parse");
        assert!(events_payload_to_parsed(payload).is_none());
    }

    #[test]
    fn parses_member_joined() {
        let payload: SlackEventPayload = serde_json::from_value(json!({
            "event": {
                "type": "member_joined_channel",
                "channel": "C1",
                "user": "U9"
            }
        }))
        .expect("parse");
        assert_eq!(
            events_payload_to_parsed(payload),
            Some(ParsedEvent::MemberJoined {
                channel_id: "C1".into(),
                user_id: "U9".into(),
            })
        );
    }
}

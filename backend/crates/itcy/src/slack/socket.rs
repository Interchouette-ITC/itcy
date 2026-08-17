// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Slack Socket Mode: apps.connections.open + WebSocket event loop.

use crate::slack::events::{events_payload_to_parsed, ParsedEvent, SlackEventPayload};
use crate::slack::handler::{event_allowed, SlackRuntime};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tracing::{debug, info, warn};

/// True when Slack (or the network) dropped the socket without a clean Close frame.
/// Common on Socket Mode; reconnect is expected, not an operator-facing failure.
fn is_expected_ws_drop(err: &WsError) -> bool {
    match err {
        WsError::ConnectionClosed | WsError::AlreadyClosed | WsError::Protocol(_) => true,
        WsError::Io(io) => matches!(
            io.kind(),
            ErrorKind::ConnectionReset
                | ErrorKind::BrokenPipe
                | ErrorKind::ConnectionAborted
                | ErrorKind::UnexpectedEof
        ),
        _ => false,
    }
}

const CONNECTIONS_OPEN_URL: &str = "https://slack.com/api/apps.connections.open";

#[derive(Debug, Deserialize)]
struct ConnectionsOpenResponse {
    ok: bool,
    url: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SocketEnvelope {
    #[serde(rename = "type")]
    envelope_type: String,
    envelope_id: Option<String>,
    payload: Option<serde_json::Value>,
}

/// Slash command payload (Socket Mode type `slash_commands`).
#[derive(Debug, Deserialize)]
struct SlashCommandPayload {
    command: Option<String>,
    text: Option<String>,
    user_id: Option<String>,
    channel_id: Option<String>,
}

async fn get_wss_url(app_token: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(CONNECTIONS_OPEN_URL)
        .bearer_auth(app_token)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("")
        .send()
        .await
        .map_err(|e| format!("apps.connections.open request: {e}"))?;

    let body: ConnectionsOpenResponse = resp
        .json()
        .await
        .map_err(|e| format!("apps.connections.open parse: {e}"))?;

    if !body.ok {
        return Err(body
            .error
            .unwrap_or_else(|| "unknown Slack API error".into()));
    }
    body.url
        .ok_or_else(|| "Slack did not return a WebSocket URL".to_string())
}

fn process_envelope(envelope: SocketEnvelope) -> (Option<String>, Option<ParsedEvent>) {
    let envelope_id = envelope.envelope_id.clone();
    let Some(payload_val) = envelope.payload else {
        return (envelope_id, None);
    };

    if envelope.envelope_type == "slash_commands" {
        let cmd: SlashCommandPayload = match serde_json::from_value(payload_val) {
            Ok(p) => p,
            Err(e) => {
                debug!(error = %e, "slack: slash payload deserialize failed");
                return (envelope_id, None);
            }
        };
        return (
            envelope_id,
            Some(ParsedEvent::SlashCommand {
                channel_id: cmd.channel_id.unwrap_or_default(),
                user_id: cmd.user_id.unwrap_or_default(),
                command: cmd.command.unwrap_or_default(),
                text: cmd.text.unwrap_or_default(),
            }),
        );
    }

    if envelope.envelope_type != "events_api" {
        return (envelope_id, None);
    }
    let payload: SlackEventPayload = match serde_json::from_value(payload_val) {
        Ok(p) => p,
        Err(e) => {
            debug!(error = %e, "slack: events payload deserialize failed");
            return (envelope_id, None);
        }
    };
    (envelope_id, events_payload_to_parsed(payload))
}

/// Runs forever: connect, handle, reconnect on disconnect.
pub async fn run_socket_mode_loop(runtime: Arc<SlackRuntime>) -> ! {
    let app_token = runtime.config.app_token.clone();
    loop {
        match get_wss_url(&app_token).await {
            Ok(url) => {
                info!("slack: Socket Mode connecting");
                match connect_async(&url).await {
                    Ok((ws_stream, _)) => {
                        info!("slack: Socket Mode connected");
                        if let Ok(mut flag) = runtime.slack_connected.lock() {
                            *flag = true;
                        }
                        let (mut write, mut read) = ws_stream.split();
                        while let Some(msg) = read.next().await {
                            match msg {
                                Ok(Message::Text(text)) => {
                                    let envelope: SocketEnvelope = match serde_json::from_str(&text)
                                    {
                                        Ok(e) => e,
                                        Err(e) => {
                                            debug!(error = %e, "slack: envelope parse failed");
                                            continue;
                                        }
                                    };
                                    let envelope_type = envelope.envelope_type.clone();
                                    if envelope_type == "hello" {
                                        continue;
                                    }
                                    if envelope_type == "disconnect" {
                                        info!("slack: Socket Mode server disconnect");
                                        break;
                                    }
                                    let (ack_id, parsed) = process_envelope(envelope);
                                    // Ack the envelope before long work so Slack does not
                                    // mark the slash as "app did not respond".
                                    if let Some(id) = ack_id {
                                        let ack = json!({ "envelope_id": id });
                                        if write
                                            .send(Message::Text(ack.to_string().into()))
                                            .await
                                            .is_err()
                                        {
                                            info!(
                                                "slack: Socket Mode ack send failed; reconnecting"
                                            );
                                            break;
                                        }
                                    }
                                    if let Some(event) = parsed {
                                        if event_allowed(runtime.as_ref(), &event) {
                                            debug!(?event, "slack: handling event");
                                            let rt = Arc::clone(&runtime);
                                            tokio::spawn(async move {
                                                rt.handle_event(event).await;
                                            });
                                        } else {
                                            debug!("slack: event filtered (wrong channel)");
                                        }
                                    }
                                }
                                Ok(Message::Ping(data)) => {
                                    let _ = write.send(Message::Pong(data)).await;
                                }
                                Ok(Message::Close(_)) => {
                                    info!("slack: Socket Mode websocket closed");
                                    break;
                                }
                                Err(e) => {
                                    if is_expected_ws_drop(&e) {
                                        info!(error = %e, "slack: Socket Mode connection dropped");
                                    } else {
                                        warn!(error = %e, "slack: websocket read error");
                                    }
                                    break;
                                }
                                _ => {}
                            }
                        }
                        if let Ok(mut flag) = runtime.slack_connected.lock() {
                            *flag = false;
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "slack: Socket Mode connect error");
                        if let Ok(mut flag) = runtime.slack_connected.lock() {
                            *flag = false;
                        }
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "slack: Socket Mode get URL error");
                if let Ok(mut flag) = runtime.slack_connected.lock() {
                    *flag = false;
                }
            }
        }
        info!("slack: Socket Mode reconnecting in 5s");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_slash_envelope() {
        let envelope = SocketEnvelope {
            envelope_type: "slash_commands".into(),
            envelope_id: Some("E1".into()),
            payload: Some(json!({
                "command": "/draft_about",
                "text": "rust async",
                "user_id": "U1",
                "channel_id": "C1"
            })),
        };
        let (id, ev) = process_envelope(envelope);
        assert_eq!(id.as_deref(), Some("E1"));
        assert_eq!(
            ev,
            Some(ParsedEvent::SlashCommand {
                channel_id: "C1".into(),
                user_id: "U1".into(),
                command: "/draft_about".into(),
                text: "rust async".into(),
            })
        );
    }

    #[test]
    fn expected_ws_drop_classifies_reset_and_protocol() {
        use std::io;
        use tokio_tungstenite::tungstenite::error::ProtocolError;

        assert!(is_expected_ws_drop(&WsError::ConnectionClosed));
        assert!(is_expected_ws_drop(&WsError::Protocol(
            ProtocolError::ResetWithoutClosingHandshake
        )));
        assert!(is_expected_ws_drop(&WsError::Io(io::Error::new(
            ErrorKind::ConnectionReset,
            "reset"
        ))));
        assert!(!is_expected_ws_drop(&WsError::Io(io::Error::new(
            ErrorKind::PermissionDenied,
            "nope"
        ))));
    }
}

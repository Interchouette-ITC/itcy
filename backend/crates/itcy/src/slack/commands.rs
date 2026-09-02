// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Operator slash commands + legacy text help.

use crate::memory::StoredMessage;

/// Parsed operator command (slash or exact text `help`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorCommand {
    Help,
    Status,
    DraftAbout {
        subject: String,
        instructions: String,
    },
    /// `LinkedIn` draft grounded in Interchouette catalog / public cites (not press corpus).
    DraftAboutItc {
        subject: String,
        instructions: String,
    },
    /// Open/update BAT PR for a `DRAFT-` or `TWEET-` id.
    Accept {
        draft_id: String,
    },
    /// Publish when Approve already happened but the webhook was missed.
    RetryBat {
        draft_id: String,
    },
    /// Rewrite a saved `DRAFT-` or `TWEET-` row.
    Rework {
        draft_id: String,
        instructions: String,
    },
    /// Swap cite / link on a saved `DRAFT-` or `TWEET-` row.
    ChangeUrl {
        draft_id: String,
        choice: String,
    },
    /// Two modes: no args = draft from corpus; `DIGEST-…` / bare N = pick digest propositions.
    ProposeDraft {
        /// `DIGEST-…` when picking; `None` for corpus (empty indices) or latest digest (bare N).
        digest_id: Option<String>,
        /// Empty with no `digest_id` = corpus draft. Otherwise 1-based digest indices.
        indices: Vec<i32>,
    },
    /// Build + post numbered subject digest to `#daily-digest`.
    DailyDigest,
    /// Draft a `LinkedIn` or X threaded reply (`CREPLY-` / `XREPLY-`).
    ReplyComment {
        url: String,
    },
    /// Tor enrich one personal `LinkedIn` post URL into corpus.
    Enrich {
        url: String,
    },
    /// Ingest one public publisher URL into corpus.
    Ingest {
        url: String,
    },
    TweetAbout {
        subject: String,
        instructions: String,
    },
    /// Joke tweet tagging `@grok` `@cursor_ai` `@elonmusk` (no SERP / cite).
    TweetFarce {
        theme: String,
    },
    /// X tweet grounded in Interchouette catalog / public cites.
    DraftTweetAboutItc {
        subject: String,
        instructions: String,
    },
    /// `LinkedIn` self-introduction post as `ITCy` (first-person voice, stack disclosure).
    DraftAboutItcy {
        instructions: String,
    },
    /// X self-introduction tweet as `ITCy` (first-person voice, stack disclosure).
    TweetAboutItcy {
        instructions: String,
    },
    ProposeTweet {
        digest_id: Option<String>,
        indices: Vec<i32>,
    },
    /// List saved drafts and tweets (not published).
    List,
    /// Show one or more saved `DRAFT-` / `TWEET-` rows, or re-post a `DIGEST-`.
    Show {
        ids: Vec<String>,
    },
    /// Delete one or more saved `DRAFT-` / `TWEET-` rows (closes open PRs).
    Delete {
        ids: Vec<String>,
    },
    /// Append a person/company to `handles.toml` and hot-reload the registry.
    HandleAdd {
        raw: String,
    },
}

/// Outcome of a slash command (immediate ack + final reply).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandOutcome {
    /// Immediate operator ack (not sent for `help` or `status_itcy`).
    pub ack: Option<String>,
    pub reply: String,
}

/// Slash command name string for logs.
#[must_use]
pub const fn slash_command_name(cmd: &OperatorCommand) -> &'static str {
    match cmd {
        OperatorCommand::Help => "help",
        OperatorCommand::Status => "status_itcy",
        OperatorCommand::DraftAbout { .. } => "/draft_about",
        OperatorCommand::DraftAboutItc { .. } => "/draft_about_itc",
        OperatorCommand::Accept { .. } => "/accept",
        OperatorCommand::RetryBat { .. } => "/retry_bat",
        OperatorCommand::Rework { .. } => "/rework",
        OperatorCommand::ChangeUrl { .. } => "/change_url",
        OperatorCommand::ProposeDraft { .. } => "/propose_draft",
        OperatorCommand::DailyDigest => "/daily_digest",
        OperatorCommand::ReplyComment { .. } => "/reply_comment",
        OperatorCommand::Enrich { .. } => "/enrich",
        OperatorCommand::Ingest { .. } => "/ingest",
        OperatorCommand::TweetAbout { .. } => "/tweet_about",
        OperatorCommand::TweetFarce { .. } => "/tweet_farce",
        OperatorCommand::DraftTweetAboutItc { .. } => "/draft_tweet_about_itc",
        OperatorCommand::DraftAboutItcy { .. } => "/draft_about_itcy",
        OperatorCommand::TweetAboutItcy { .. } => "/tweet_about_itcy",
        OperatorCommand::ProposeTweet { .. } => "/propose_tweet",
        OperatorCommand::List => "/list",
        OperatorCommand::Show { .. } => "/show",
        OperatorCommand::Delete { .. } => "/delete",
        OperatorCommand::HandleAdd { .. } => "/handle_add",
    }
}

/// Registered Slack slash names (no leading `/`).
///
/// Single catalog for inline channel detection and drift checks against
/// [`parse_slash_command`]. `/status` is included so the reserved-workspace
/// error is returned instead of freeform.
pub const KNOWN_SLASH_COMMANDS: &[&str] = &[
    "help",
    "status_itcy",
    "status",
    "list",
    "show",
    "delete",
    "accept",
    "rework",
    "change_url",
    "propose_draft",
    "propose_tweet",
    "draft_about",
    "draft_about_itc",
    "draft_about_itcy",
    "draft_tweet_about_itc",
    "tweet_about_itcy",
    "tweet_about",
    "tweet_farce",
    "retry_bat",
    "daily_digest",
    "reply_comment",
    "enrich",
    "ingest",
    "handle_add",
];

/// True when `name` is in [`KNOWN_SLASH_COMMANDS`] (leading `/` ignored).
#[must_use]
pub fn is_known_slash_command(name: &str) -> bool {
    let n = name.trim().trim_start_matches('/').to_ascii_lowercase();
    KNOWN_SLASH_COMMANDS.iter().any(|&c| c == n)
}

/// True when the slash handler should post an immediate ack before running work.
#[must_use]
pub fn command_needs_ack(cmd: &OperatorCommand) -> bool {
    match cmd {
        OperatorCommand::Help | OperatorCommand::Status | OperatorCommand::List => false,
        OperatorCommand::Show { ids } => ids.iter().any(|id| id.starts_with("DIGEST-")),
        _ => true,
    }
}

/// Immediate operator ack for Socket Mode. Must be posted **before** long work starts.
#[must_use]
pub fn slash_immediate_ack(cmd: &OperatorCommand) -> Option<String> {
    if command_needs_ack(cmd) {
        Some(command_ack_text(cmd))
    } else {
        None
    }
}

/// Ordered channel posts for one slash: optional ack first, then final reply.
///
/// Regression contract: ack is never after the reply. Socket Mode must post in this order
/// (ack before `dispatch_command`; reply after). Inject `/entrypoint/slash` returns both in JSON
/// and does not post to Slack.
#[must_use]
pub fn slash_channel_post_sequence(ack: Option<String>, reply: String) -> Vec<String> {
    let mut posts = Vec::with_capacity(2);
    if let Some(a) = ack {
        posts.push(a);
    }
    posts.push(reply);
    posts
}

/// One-line log headline from a Slack slash reply (never a mid-line rip of bullets).
#[must_use]
pub fn slash_reply_headline(reply: &str) -> String {
    let first = reply
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let cleaned = first.trim_matches(|c| c == '*' || c == '`' || c == '_');
    cleaned.chars().take(100).collect()
}

/// Short Slack ack posted before long-running slash work starts.
///
/// Plain text only (no backticks / italics). Args stay visible; join subject and
/// instructions with `, ` (never ` | `).
#[must_use]
pub fn command_ack_text(cmd: &OperatorCommand) -> String {
    match cmd {
        OperatorCommand::Help | OperatorCommand::Status => String::new(),
        OperatorCommand::DraftAbout {
            subject,
            instructions,
        } => ack_subject_cmd("/draft_about", subject, instructions),
        OperatorCommand::DraftAboutItc {
            subject,
            instructions,
        } => ack_itc_subject("/draft_about_itc", subject, instructions),
        OperatorCommand::Accept { draft_id } => {
            format!("Received /accept {draft_id}")
        }
        OperatorCommand::RetryBat { draft_id } => {
            format!("Received /retry_bat {draft_id}")
        }
        OperatorCommand::Rework {
            draft_id,
            instructions,
        } => {
            if instructions.is_empty() {
                format!("Received /rework {draft_id}")
            } else {
                format!("Received /rework {draft_id}, {instructions}")
            }
        }
        OperatorCommand::ChangeUrl { draft_id, choice } => {
            format!("Received /change_url {draft_id}, {choice}")
        }
        OperatorCommand::ProposeDraft { digest_id, indices } => {
            ack_propose("/propose_draft", digest_id.as_deref(), indices)
        }
        OperatorCommand::DailyDigest => "Received /daily_digest".into(),
        OperatorCommand::ReplyComment { url } => {
            format!("Received /reply_comment {url}")
        }
        OperatorCommand::Enrich { url } => format!("Received /enrich {url}"),
        OperatorCommand::Ingest { url } => format!("Received /ingest {url}"),
        OperatorCommand::HandleAdd { raw } => {
            let preview = raw.chars().take(80).collect::<String>();
            format!("Received /handle_add {preview}")
        }
        OperatorCommand::TweetAbout {
            subject,
            instructions,
        } => ack_subject_cmd("/tweet_about", subject, instructions),
        OperatorCommand::TweetFarce { theme } => {
            if theme.trim().is_empty() {
                "Received /tweet_farce".into()
            } else {
                let preview: String = theme.chars().take(80).collect();
                format!("Received /tweet_farce {preview}")
            }
        }
        OperatorCommand::DraftTweetAboutItc {
            subject,
            instructions,
        } => ack_itc_subject("/draft_tweet_about_itc", subject, instructions),
        OperatorCommand::DraftAboutItcy { instructions } => {
            if instructions.is_empty() {
                "Received /draft_about_itcy".into()
            } else {
                format!("Received /draft_about_itcy {instructions}")
            }
        }
        OperatorCommand::TweetAboutItcy { instructions } => {
            if instructions.is_empty() {
                "Received /tweet_about_itcy".into()
            } else {
                format!("Received /tweet_about_itcy {instructions}")
            }
        }
        OperatorCommand::ProposeTweet { digest_id, indices } => {
            ack_propose("/propose_tweet", digest_id.as_deref(), indices)
        }
        OperatorCommand::List => "Received /list".into(),
        OperatorCommand::Show { ids } => ack_ids("/show", ids),
        OperatorCommand::Delete { ids } => ack_ids("/delete", ids),
    }
}

fn ack_ids(cmd: &str, ids: &[String]) -> String {
    let listed = ids.join(", ");
    format!("Received {cmd} {listed}")
}

fn ack_subject_cmd(cmd: &str, subject: &str, instructions: &str) -> String {
    if instructions.is_empty() {
        format!("Received {cmd} {subject}")
    } else {
        format!("Received {cmd} {subject}, {instructions}")
    }
}

fn ack_itc_subject(cmd: &str, subject: &str, instructions: &str) -> String {
    if subject.is_empty() {
        format!("Received {cmd}")
    } else {
        ack_subject_cmd(cmd, subject, instructions)
    }
}

fn ack_propose(cmd: &str, digest_id: Option<&str>, indices: &[i32]) -> String {
    if digest_id.is_none() && indices.is_empty() {
        return format!("Received {cmd}");
    }
    let idx = indices
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    match digest_id {
        Some(id) if indices.is_empty() => format!("Received {cmd} {id}"),
        Some(id) => format!("Received {cmd} {id}, {idx}"),
        None => format!("Received {cmd} {idx}"),
    }
}

/// Strips a leading `<@BOTID>` mention and trims.
#[must_use]
pub fn normalize_user_text(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("<@") {
        if let Some(idx) = rest.find('>') {
            return rest[idx + 1..].trim().to_string();
        }
    }
    trimmed.to_string()
}

/// Maps exact channel text to help/status only (workflows are slash).
#[must_use]
pub fn parse_text_command(text: &str) -> Option<OperatorCommand> {
    let normalized = normalize_user_text(text).to_ascii_lowercase();
    match normalized.as_str() {
        "help" | "commands" => Some(OperatorCommand::Help),
        "status_itcy" | "status itcy" => Some(OperatorCommand::Status),
        _ => None,
    }
}

/// If channel text contains a **known** `ITCy` `/cmd`, treat from that token as a slash.
///
/// Leading prose before the first known `/cmd` is ignored. Unknown `/tokens` are
/// ignored (freeform). Parse / usage errors for known cmds become `Some(Err(...))`
/// so the caller still skips freeform LLM.
#[must_use]
pub fn parse_inline_slash_text(text: &str) -> Option<Result<OperatorCommand, String>> {
    let normalized = normalize_user_text(text);
    let (cmd, args) = extract_inline_slash(&normalized)?;
    Some(parse_slash_command(&cmd, &args))
}

/// How a channel / e2e text message should be handled before any freeform LLM call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelTextKind {
    /// Exact `help` / `status_itcy`.
    TextCommand(OperatorCommand),
    /// First known `/cmd` in the message (Ok = dispatch, Err = usage, never freeform).
    InlineSlash(Result<OperatorCommand, String>),
    /// No known slash / text command → freeform LLM path.
    Freeform,
}

/// Classifies channel text: text command, inline slash, or freeform.
#[must_use]
pub fn classify_channel_text(text: &str) -> ChannelTextKind {
    if let Some(cmd) = parse_text_command(text) {
        return ChannelTextKind::TextCommand(cmd);
    }
    if let Some(parsed) = parse_inline_slash_text(text) {
        return ChannelTextKind::InlineSlash(parsed);
    }
    ChannelTextKind::Freeform
}

/// First **known** `/cmd` in `text` → (`/cmd`, remaining args).
///
/// Skips URL path noise, unknown `/token`s, and operator **help chrome** lines
/// (`/change_url … <0|1|2|3|4|5|url>`, `/rework … <instructions>`) so pasted draft
/// footers do not dispatch as live commands.
fn extract_inline_slash(text: &str) -> Option<(String, String)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'/' {
            i += 1;
            continue;
        }
        let name_start = i + 1;
        let mut name_end = name_start;
        while name_end < bytes.len() {
            let c = bytes[name_end];
            if c.is_ascii_alphanumeric() || c == b'_' {
                name_end += 1;
            } else {
                break;
            }
        }
        if name_end == name_start {
            i += 1;
            continue;
        }
        // Avoid matching URL paths (`https://…`, `example.com/path`).
        if i > 0 {
            let prev = bytes[i - 1];
            if prev.is_ascii_alphanumeric() || prev == b':' || prev == b'/' {
                i += 1;
                continue;
            }
        }
        let name = &text[name_start..name_end];
        if !is_known_slash_command(name) {
            i = name_end;
            continue;
        }
        let args = text[name_end..].trim().to_string();
        if slash_args_look_like_usage_chrome(name, &args) {
            i = name_end;
            continue;
        }
        let cmd = text[i..name_end].to_string();
        return Some((cmd, args));
    }
    None
}

/// True when args are the bot's usage placeholder, not an operator choice.
fn slash_args_look_like_usage_chrome(cmd: &str, args: &str) -> bool {
    match cmd {
        "change_url" => args.contains("<0|1|2|3"),
        // Bot Next: footers still show `<instructions>` as a placeholder token.
        "rework" => args.contains('<') && args.contains("instructions"),
        _ => false,
    }
}

/// Pasted draft / `/show` chrome: do not treat as operator input (inline slash or freeform).
#[must_use]
pub fn looks_like_bot_draft_paste(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("Received /")
        || t.contains("0 = no link")
        || t.contains("Written by AI - ITCy")
        || t.contains(":point_right: Next:")
        || t.contains("Draft ID:")
        || t.contains("Tweet ID:")
}

/// Split `/draft_about` args: `subject, instructions` (first comma).
#[must_use]
pub fn parse_draft_about_args(text: &str) -> (String, String) {
    let t = text.trim();
    if let Some((left, right)) = t.split_once(',') {
        (left.trim().to_string(), right.trim().to_string())
    } else {
        (t.to_string(), String::new())
    }
}

/// Full operator brief for LOAD/DRAFT (subject + optional instructions).
#[must_use]
pub fn compose_operator_brief(subject: &str, instructions: &str) -> String {
    let s = subject.trim();
    let i = instructions.trim();
    if i.is_empty() {
        s.to_string()
    } else {
        format!("{s}, {i}")
    }
}

/// Maps a Slack slash `command` + `text` args.
///
/// # Errors
///
/// Returns `Err(String)` with an operator-facing message when validation or lookup fails.
pub fn parse_slash_command(command: &str, text: &str) -> Result<OperatorCommand, String> {
    let cmd = command.trim().trim_start_matches('/').to_ascii_lowercase();
    let args = text.trim();
    if let Some(saved) = parse_saved_slash(&cmd, args) {
        return saved;
    }
    if let Some(life) = parse_lifecycle_slash(&cmd, args) {
        return life;
    }
    if let Some(tweet) = parse_tweet_slash(&cmd, args) {
        return tweet;
    }
    if let Some(itc) = parse_itc_draft_slash(&cmd, args) {
        return itc;
    }
    if let Some(li) = parse_linkedin_slash(&cmd, args) {
        return li;
    }
    match cmd.as_str() {
        "help" => Ok(OperatorCommand::Help),
        "status_itcy" => Ok(OperatorCommand::Status),
        "daily_digest" => Ok(OperatorCommand::DailyDigest),
        "reply_comment" => {
            let url = parse_slash_url(args)?;
            Ok(OperatorCommand::ReplyComment { url })
        }
        "enrich" => {
            let url = parse_slash_url(args)?;
            if !crate::sources::enrich::is_linkedin_enrich_url(&url) {
                return Err(
                    "usage: /enrich <linkedin post url> (post or activity URL only)".into(),
                );
            }
            Ok(OperatorCommand::Enrich { url })
        }
        "ingest" => {
            let url = parse_slash_url(args)?;
            if crate::sources::url_hygiene::is_linkedin_pulse_url(&url) {
                let url = crate::sources::url_hygiene::canonicalize_ingest_url(&url);
                return Ok(OperatorCommand::Ingest { url });
            }
            if is_linkedin_host(&url) {
                return Err(
                    "LinkedIn posts/activity use `/enrich` (Tor). `/ingest` accepts publisher pages or LinkedIn Pulse articles (clearnet)."
                        .into(),
                );
            }
            Ok(OperatorCommand::Ingest {
                url: crate::sources::url_hygiene::canonicalize_ingest_url(&url),
            })
        }
        "handle_add" => {
            if args.is_empty() {
                return Err(
                    "usage: /handle_add <name> <@linkedin|url> <@x|url>".into(),
                );
            }
            Ok(OperatorCommand::HandleAdd {
                raw: args.to_string(),
            })
        }
        "status" => Err(
            "`/status` is reserved in this workspace. Type `status_itcy` for ITCy process snapshot."
                .into(),
        ),
        other => Err(format!(
            "unknown slash `/{other}`. Type `help` for the operator command list."
        )),
    }
}

fn parse_linkedin_slash(cmd: &str, args: &str) -> Option<Result<OperatorCommand, String>> {
    Some(match cmd {
        "draft_about" => {
            if args.is_empty() {
                return Some(Err("usage: /draft_about <subject>, <instructions>".into()));
            }
            let (subject, instructions) = parse_draft_about_args(args);
            if subject.is_empty() {
                return Some(Err("usage: /draft_about <subject>, <instructions>".into()));
            }
            Ok(OperatorCommand::DraftAbout {
                subject,
                instructions,
            })
        }
        "retry_bat" => {
            let Some(id) = extract_artefact_id(args) else {
                return Some(Err(
                    "usage: /retry_bat <DRAFT-…|POST-…|TWEET-…|XPOST-…>".into()
                ));
            };
            Ok(OperatorCommand::RetryBat { draft_id: id })
        }
        "propose_draft" => parse_propose_draft_args(args),
        _ => return None,
    })
}

fn parse_saved_slash(cmd: &str, args: &str) -> Option<Result<OperatorCommand, String>> {
    Some(match cmd {
        "list" => Ok(OperatorCommand::List),
        "show" => parse_show_ids_cmd(args),
        "delete" => parse_saved_ids_cmd(
            args,
            "usage: /delete <DRAFT-…|POST-…|TWEET-…|XPOST-…|CREPLY-…|XREPLY-…>[, <ID>…]",
            |ids| OperatorCommand::Delete { ids },
        ),
        _ => return None,
    })
}

fn parse_lifecycle_slash(cmd: &str, args: &str) -> Option<Result<OperatorCommand, String>> {
    Some(match cmd {
        "accept" => {
            let Some(id) = extract_draft_or_tweet_id(args) else {
                return Some(Err(
                    "usage: /accept <DRAFT-…|TWEET-…|CREPLY-…|XREPLY-…>".into()
                ));
            };
            Ok(OperatorCommand::Accept { draft_id: id })
        }
        "rework" => {
            let Some((id, rest)) = split_draft_or_tweet_id_and_rest(args) else {
                return Some(Err(
                    "usage: /rework <DRAFT-…|TWEET-…|CREPLY-…|XREPLY-…> [instructions|paste]"
                        .into(),
                ));
            };
            Ok(OperatorCommand::Rework {
                draft_id: id,
                instructions: crate::sources::rework::sanitize_rework_instructions(&rest),
            })
        }
        "change_url" => {
            let Some((id, rest)) = split_draft_or_tweet_id_and_rest(args) else {
                return Some(Err(
                    "usage: /change_url <DRAFT-…|TWEET-…> <0|1|2|3|4|5|https://…>".into(),
                ));
            };
            if crate::sources::reply_comment::is_reply_id(&id) {
                return Some(Err(
                    "replies have no cite URL; use `/rework` then `/accept`".into(),
                ));
            }
            // One token only: pasted Next: footers append more slash lines after `1`.
            let choice = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| matches!(c, ',' | ':' | '.'))
                .to_string();
            if choice.is_empty() {
                return Some(Err(
                    "usage: /change_url <DRAFT-…|TWEET-…> <0|1|2|3|4|5|https://…>".into(),
                ));
            }
            Ok(OperatorCommand::ChangeUrl {
                draft_id: id,
                choice,
            })
        }
        _ => return None,
    })
}

fn parse_itc_draft_slash(cmd: &str, args: &str) -> Option<Result<OperatorCommand, String>> {
    match cmd {
        "draft_about_itc" => {
            let (subject, instructions) = if args.is_empty() {
                (String::new(), String::new())
            } else {
                parse_draft_about_args(args)
            };
            Some(Ok(OperatorCommand::DraftAboutItc {
                subject,
                instructions,
            }))
        }
        "draft_about_itcy" => {
            let instructions = args.trim().to_string();
            Some(Ok(OperatorCommand::DraftAboutItcy { instructions }))
        }
        "tweet_about_itcy" => {
            let instructions = args.trim().to_string();
            Some(Ok(OperatorCommand::TweetAboutItcy { instructions }))
        }
        _ => None,
    }
}

fn parse_tweet_slash(cmd: &str, args: &str) -> Option<Result<OperatorCommand, String>> {
    Some(match cmd {
        "tweet_about" => {
            if args.is_empty() {
                return Some(Err("usage: /tweet_about <subject>, <instructions>".into()));
            }
            let (subject, instructions) = parse_draft_about_args(args);
            if subject.is_empty() {
                return Some(Err("usage: /tweet_about <subject>, <instructions>".into()));
            }
            Ok(OperatorCommand::TweetAbout {
                subject,
                instructions,
            })
        }
        "tweet_farce" => Ok(OperatorCommand::TweetFarce {
            theme: args.trim().to_string(),
        }),
        "draft_tweet_about_itc" => {
            let (subject, instructions) = if args.is_empty() {
                (String::new(), String::new())
            } else {
                parse_draft_about_args(args)
            };
            Ok(OperatorCommand::DraftTweetAboutItc {
                subject,
                instructions,
            })
        }
        "propose_tweet" => parse_propose_tweet_args(args),
        _ => return None,
    })
}

fn parse_slash_url(args: &str) -> Result<String, String> {
    let t = args.trim();
    if t.is_empty() {
        return Err("usage: pass one https:// URL".into());
    }
    let url = if t.starts_with("http://") || t.starts_with("https://") {
        t.trim_end_matches(['.', ',', ')', ']']).to_string()
    } else {
        return Err("usage: pass one https:// URL".into());
    };
    Ok(url)
}

/// `/propose_draft` (corpus) | `/propose_draft DIGEST-…, 1,3` | `/propose_draft 3` (top N of latest digest).
fn parse_propose_draft_args(args: &str) -> Result<OperatorCommand, String> {
    let usage =
        "usage: /propose_draft  |  /propose_draft <DIGEST-…>, <1|1,3>  |  /propose_draft <N>";
    // Slack code spans / fences make paste ugly; strip before parse.
    let cleaned = args.replace('`', "");
    let t = cleaned.trim().trim_start_matches('#');
    if t.is_empty() {
        return Ok(OperatorCommand::ProposeDraft {
            digest_id: None,
            indices: Vec::new(),
        });
    }
    if let Some(id) = digest_id_from_token(t.split([',', ' ']).next().unwrap_or("")) {
        let rest = t
            .find(&id)
            .map_or("", |i| {
                t[i + id.len()..].trim_start_matches([',', ' ', '#'])
            })
            .trim();
        let indices = parse_index_list(rest)?;
        if indices.is_empty() {
            return Err(usage.into());
        }
        return Ok(OperatorCommand::ProposeDraft {
            digest_id: Some(id),
            indices,
        });
    }
    // Bare N = top N from latest open digest.
    let n: i32 = t
        .split_whitespace()
        .next()
        .and_then(|s| s.replace('`', "").parse().ok())
        .filter(|&n| n > 0)
        .ok_or_else(|| usage.to_string())?;
    Ok(OperatorCommand::ProposeDraft {
        digest_id: None,
        indices: (1..=n).collect(),
    })
}

fn parse_propose_tweet_args(args: &str) -> Result<OperatorCommand, String> {
    match parse_propose_draft_args(args)? {
        OperatorCommand::ProposeDraft { digest_id, indices } => {
            Ok(OperatorCommand::ProposeTweet { digest_id, indices })
        }
        _ => Err(
            "usage: /propose_tweet  |  /propose_tweet <DIGEST-…>, <1|1,3>  |  /propose_tweet <N>"
                .into(),
        ),
    }
}

fn parse_index_list(raw: &str) -> Result<Vec<i32>, String> {
    let mut out = Vec::new();
    for part in raw.split([',', ' ']) {
        let p = part
            .trim()
            .trim_start_matches(['#', '`'])
            .trim_end_matches('`');
        if p.is_empty() {
            continue;
        }
        let n: i32 = p
            .parse()
            .map_err(|_| format!("bad digest item index {p}"))?;
        if n < 1 {
            return Err(format!("digest item index must be >= 1 (got {n})"));
        }
        if !out.contains(&n) {
            out.push(n);
        }
    }
    Ok(out)
}

fn digest_id_from_token(raw: &str) -> Option<String> {
    let t = normalize_draft_id(raw)
        .trim_start_matches('#')
        .trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .to_string();
    if t.starts_with("DIGEST-") && t.len() > "DIGEST-".len() {
        Some(t)
    } else {
        None
    }
}

fn is_linkedin_host(url: &str) -> bool {
    crate::sources::url_hygiene::is_linkedin_host(url)
}

/// Pull `DRAFT-…` out of a token (strips `Draft ID:`, quotes, trailing punctuation).
fn draft_id_from_token(raw: &str) -> Option<String> {
    let mut t = normalize_draft_id(raw);
    if let Some(rest) = t.strip_prefix("ID:") {
        t = normalize_draft_id(rest);
    } else if let Some(rest) = t.strip_prefix("id:") {
        t = normalize_draft_id(rest);
    }
    let t = t.trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-'));
    if t.starts_with("DRAFT-") && t.len() > "DRAFT-".len() {
        Some(t.to_string())
    } else {
        None
    }
}

fn tweet_id_from_token(raw: &str) -> Option<String> {
    let mut t = normalize_draft_id(raw);
    if let Some(rest) = t.strip_prefix("ID:") {
        t = normalize_draft_id(rest);
    } else if let Some(rest) = t.strip_prefix("id:") {
        t = normalize_draft_id(rest);
    }
    let t = t.trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-'));
    if t.starts_with("TWEET-") && t.len() > "TWEET-".len() {
        Some(t.to_string())
    } else {
        None
    }
}

fn parse_saved_ids_cmd(
    args: &str,
    usage: &str,
    make: fn(Vec<String>) -> OperatorCommand,
) -> Result<OperatorCommand, String> {
    let ids = collect_ids(args, draft_tweet_or_post_id_from_token);
    if ids.is_empty() {
        return Err(usage.into());
    }
    Ok(make(ids))
}

fn parse_show_ids_cmd(args: &str) -> Result<OperatorCommand, String> {
    const USAGE: &str =
        "usage: /show <DRAFT-…|POST-…|TWEET-…|XPOST-…|CREPLY-…|XREPLY-…|DIGEST-…>[, <ID>…]";
    let ids = collect_ids(args, draft_tweet_post_or_digest_id_from_token);
    if ids.is_empty() {
        return Err(USAGE.into());
    }
    Ok(OperatorCommand::Show { ids })
}

fn draft_tweet_post_or_digest_id_from_token(raw: &str) -> Option<String> {
    draft_tweet_or_post_id_from_token(raw).or_else(|| digest_id_from_token(raw))
}

fn draft_tweet_or_post_id_from_token(raw: &str) -> Option<String> {
    draft_or_tweet_id_from_token(raw)
        .or_else(|| id_from_token(raw, "POST-"))
        .or_else(|| id_from_token(raw, "XPOST-"))
}

const MAX_SAVED_IDS: usize = 20;

fn collect_ids(args: &str, parse_one: fn(&str) -> Option<String>) -> Vec<String> {
    let mut out = Vec::new();
    for raw in args.split([',', ' ', '\t', '\n']) {
        let Some(id) = parse_one(raw) else {
            continue;
        };
        if out.contains(&id) {
            continue;
        }
        out.push(id);
        if out.len() == MAX_SAVED_IDS {
            break;
        }
    }
    out
}

fn draft_or_tweet_id_from_token(raw: &str) -> Option<String> {
    draft_id_from_token(raw)
        .or_else(|| tweet_id_from_token(raw))
        .or_else(|| id_from_token(raw, "CREPLY-"))
        .or_else(|| id_from_token(raw, "XREPLY-"))
}

/// First `DRAFT-…` or `TWEET-…` in slash args.
fn extract_draft_or_tweet_id(args: &str) -> Option<String> {
    args.split_whitespace()
        .find_map(draft_or_tweet_id_from_token)
}

fn split_draft_or_tweet_id_and_rest(args: &str) -> Option<(String, String)> {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        if let Some(id) = draft_or_tweet_id_from_token(tok) {
            let rest = tokens[i + 1..].join(" ");
            return Some((id, rest));
        }
    }
    None
}

fn extract_artefact_id(args: &str) -> Option<String> {
    args.split_whitespace().find_map(|tok| {
        tweet_id_from_token(tok)
            .or_else(|| id_from_token(tok, "XPOST-"))
            .or_else(|| extract_draft_id(tok))
            .or_else(|| id_from_token(tok, "POST-"))
    })
}

fn id_from_token(raw: &str, prefix: &str) -> Option<String> {
    let mut t = normalize_draft_id(raw);
    if let Some(rest) = t.strip_prefix("ID:") {
        t = normalize_draft_id(rest);
    } else if let Some(rest) = t.strip_prefix("id:") {
        t = normalize_draft_id(rest);
    }
    let t = t.trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-'));
    if t.starts_with(prefix) && t.len() > prefix.len() {
        Some(t.to_string())
    } else {
        None
    }
}

/// First `DRAFT-…` in free text / slash args (skips leading `Draft` / `ID:` paste noise).
#[must_use]
pub fn find_draft_id_in_text(text: &str) -> Option<String> {
    extract_draft_id(text)
}

/// First `DRAFT-…` in slash args (skips leading `Draft` / `ID:` paste noise).
fn extract_draft_id(args: &str) -> Option<String> {
    args.split_whitespace().find_map(draft_id_from_token)
}

fn normalize_draft_id(raw: &str) -> String {
    raw.trim().trim_matches('`').trim_matches('"').to_string()
}

/// Help text for the `#itcy` runtime channel (slash + keywords).
#[must_use]
pub const fn help_text() -> &'static str {
    "ITCy runtime (`#itcy`).\n\
     *Keywords (type in channel):*\n\
     • `help` / `commands` - this list\n\
     • `status_itcy` - process / routes / health snapshot\n\
     *Slash workflows:*\n\
     • `/draft_about <subject>, <instructions>` - draft; a https in instructions is the in-post cite\n\
     • `/draft_about_itc` or `/draft_about_itc <subject>, <instructions>` - LinkedIn draft about Interchouette / our projects\n\
     • `/draft_about_itcy` or `/draft_about_itcy <instructions>` - LinkedIn self-introduction post as ITCy (first-person, stack disclosure)\n\
     • `/rework <Draft-ID|Tweet-ID|Reply-ID> [instructions|paste]` - refresh, edit, or replace saved draft/tweet/reply\n\
     • `/change_url <Draft-ID|Tweet-ID> <0|1|2|3|4|5|https://…>` - set the link (`1`–`5` or URL); `0` = no link (not for replies)\n\
     • `/accept <Draft-ID|Tweet-ID|Reply-ID>` - BAT PR for DRAFT/TWEET; direct ship for CREPLY/XREPLY\n\
     • `/list` - list drafts, tweets, replies, and shipped Posts / X posts\n\
     • `/show <Draft-ID|Post-ID|Tweet-ID|XPOST-ID|CREPLY-…|XREPLY-…|DIGEST-…>[, <ID>]` - show body/paste, or re-post a stored digest to `#daily-digest`\n\
     • `/delete <Draft-ID|Tweet-ID|CREPLY-…|XREPLY-…>[, <ID>]` - delete saved row(s) and close GitHub PRs if open\n\
     • `/retry_bat <Draft-ID|Tweet-ID|XPOST-ID>` - re-ship after BAT (missed webhook or X/LinkedIn ship failed)\n\
     • `/enrich <url>` - enrich corpus with a personal LinkedIn post (Tor)\n\
     • `/ingest <url>` - ingest public article or LinkedIn Pulse (clearnet)\n\
     • `/handle_add <name> <@linkedin|url> <@x|url>` - append LinkedIn/X handles (last two must be @ or https; hot reload)\n\
     • `/daily_digest` - 20 press + 20 For you + 20 Following + 20 tweet searches + 10 Interchouette (5 draft / 5 tweet) into `#daily-digest`\n\
     • `/propose_draft` - new draft from corpus (resolves a subject from LinkedIn/corpus memory)\n\
     • `/propose_draft <DIGEST-…>, <1|1,3>` or `/propose_draft <N>` - new drafts from that digest's propositions\n\
     • `/tweet_about <subject>, <instructions>` - tweet; a https in instructions locks the quote (X status) or the link (publisher)\n\
     • `/tweet_farce` or `/tweet_farce <theme hint>` - dad-joke / IT wordplay tagging @grok @cursor_ai @elonmusk (no cite)\n\
     • `/draft_tweet_about_itc` or `/draft_tweet_about_itc <subject>, <instructions>` - X tweet about Interchouette / our projects\n\
     • `/tweet_about_itcy` or `/tweet_about_itcy <instructions>` - X self-introduction tweet as ITCy (first-person, stack disclosure)\n\
     • `/propose_tweet` - new tweet from corpus (resolves a subject from LinkedIn/corpus memory)\n\
     • `/propose_tweet <DIGEST-…>, <1|1,3>` or `/propose_tweet <N>` - new tweets from that digest's propositions\n\
     • `/reply_comment <https://…>` - LinkedIn comment or X status → saved CREPLY-/XREPLY- draft; `/accept` ships (no BAT PR)\n\
     *Freeform chat:* anything else (informal / informational; tools OK). No draft/BAT/corpus ingest here."
}

/// Inputs for the `status` reply (keeps the formatter under clippy's arg limit).
#[derive(Debug, Clone)]
pub struct StatusSnapshot<'a> {
    pub bind: &'a str,
    pub channel_id: &'a str,
    pub slack_connected: bool,
    pub max_context_messages: u32,
    pub recent: &'a [StoredMessage],
    pub providers: &'a str,
    pub freeform_route_head: &'a str,
    pub freeform_route: &'a str,
    pub load_route_head: &'a str,
    pub load_route: &'a str,
    pub draft_route_head: &'a str,
    pub draft_route: &'a str,
    pub source_count: u64,
    pub linkedin_publish_mode: &'a str,
    pub x_publish_mode: &'a str,
    /// `LinkedIn` MCP listen detail (`ok` or down reason).
    pub linkedin_mcp: &'a str,
}

/// Builds a status reply (no secrets).
#[must_use]
pub fn status_text(snap: &StatusSnapshot<'_>) -> String {
    let providers = if snap.providers.is_empty() {
        "(empty)"
    } else {
        snap.providers
    };
    format!(
        "ITCy status\n\
         • health listener: `{bind}`\n\
         • Slack Socket Mode: {slack}\n\
         • channel id: `{channel}`\n\
         • memory window: last {max_ctx} (stored turns here: {turns})\n\
         • sources stored: {sources}\n\
         • providers (pool): {providers}\n\
         • freeform route head: `{freeform_head}`\n\
         • freeform route: {freeform_route}\n\
         • load route head: `{load_head}`\n\
         • load route: {load_route}\n\
         • draft route head: `{draft_head}`\n\
         • draft route: {draft_route}\n\
         • LinkedIn publish: `{li_mode}`\n\
         • LinkedIn MCP: `{li_mcp}`\n\
         • X publish: `{x_mode}`\n\
         • role: runtime only (slash workflows + freeform chat)",
        bind = snap.bind,
        slack = if snap.slack_connected {
            "connected"
        } else {
            "starting / reconnecting"
        },
        channel = snap.channel_id,
        max_ctx = snap.max_context_messages,
        turns = snap.recent.len(),
        sources = snap.source_count,
        freeform_head = snap.freeform_route_head,
        freeform_route = snap.freeform_route,
        load_head = snap.load_route_head,
        load_route = snap.load_route,
        draft_head = snap.draft_route_head,
        draft_route = snap.draft_route,
        li_mode = snap.linkedin_publish_mode,
        li_mcp = snap.linkedin_mcp,
        x_mode = snap.x_publish_mode,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_help_text_only() {
        assert_eq!(parse_text_command("help"), Some(OperatorCommand::Help));
        assert_eq!(
            parse_text_command("status_itcy"),
            Some(OperatorCommand::Status)
        );
        assert_eq!(parse_text_command("status"), None);
        assert_eq!(parse_text_command("hello"), None);
    }

    #[test]
    fn inline_slash_skips_change_url_usage_chrome() {
        let paste = "0 = no link. /change_url DRAFT-20260825-000102 <0|1|2|3|4|5|url>\n\
1. https://sourcegraph.com/blog/agentic-coding\n\
:link: /change_url DRAFT-20260825-000102 1\n\
:white_check_mark: /accept DRAFT-20260825-000102";
        let r = parse_inline_slash_text(paste)
            .expect("real Next hint present")
            .expect("valid change_url 1");
        match r {
            OperatorCommand::ChangeUrl { draft_id, choice } => {
                assert_eq!(draft_id, "DRAFT-20260825-000102");
                assert_eq!(choice, "1");
            }
            other => panic!("expected ChangeUrl 1, got {other:?}"),
        }
    }

    #[test]
    fn bot_slash_ack_echo_is_paste_chrome() {
        assert!(looks_like_bot_draft_paste(
            "Received /accept DRAFT-20260828-000122"
        ));
        assert!(looks_like_bot_draft_paste(
            "Received /change_url DRAFT-20260828-000122, 5"
        ));
    }

    #[test]
    fn inline_slash_skips_usage_when_only_chrome() {
        let paste = "Received /change_url DRAFT-20260825-000102, <0|1|2|3|4|5|url> :one: :dart:\n\
Written by AI - ITCy - model ollama/qwen3:8b\n\
:point_right: Next: :pencil2: /rework DRAFT-20260825-000102 <instructions>";
        assert!(parse_inline_slash_text(paste).is_none());
        assert!(looks_like_bot_draft_paste(paste));
    }

    #[test]
    fn draft_paste_chrome_is_detected() {
        assert!(looks_like_bot_draft_paste(
            "Draft ID: DRAFT-1\n\nHi\n\n0 = no link. /change_url DRAFT-1 <0|1|2|3|4|5|url>"
        ));
        assert!(!looks_like_bot_draft_paste(
            "/change_url DRAFT-20260825-000102 1"
        ));
    }

    #[test]
    fn bot_draft_paste_with_real_accept_line_is_still_detected() {
        let paste = "Draft ID: DRAFT-20260825-000102\n\n\
Amp commentary here.\n\n\
0 = no link. /change_url DRAFT-20260825-000102 <0|1|2|3|4|5|url>\n\
1. https://sourcegraph.com/blog/agentic-coding\n\
:white_check_mark: /accept DRAFT-20260825-000102";
        assert!(
            looks_like_bot_draft_paste(paste),
            "full bot paste with a real /accept line must still be chrome"
        );
    }

    #[test]
    fn change_url_choice_is_single_token_ignores_trailing_next() {
        match parse_slash_command(
            "/change_url",
            "DRAFT-20260825-000102 1 :point_right: Next: /rework DRAFT-20260825-000102 shorter",
        )
        .expect("parse")
        {
            OperatorCommand::ChangeUrl { draft_id, choice } => {
                assert_eq!(draft_id, "DRAFT-20260825-000102");
                assert_eq!(choice, "1");
            }
            other => panic!("expected ChangeUrl 1, got {other:?}"),
        }
    }

    #[test]
    fn inline_slash_from_prose_use_propose_draft() {
        let r = parse_inline_slash_text("Use /propose_draft DIGEST-20260815-000012, 11")
            .expect("slash present")
            .expect("valid propose_draft");
        match r {
            OperatorCommand::ProposeDraft { digest_id, indices } => {
                assert_eq!(digest_id.as_deref(), Some("DIGEST-20260815-000012"));
                assert_eq!(indices, vec![11]);
            }
            other => panic!("expected ProposeDraft, got {other:?}"),
        }
    }

    #[test]
    fn inline_slash_mid_sentence_rework() {
        let r = parse_inline_slash_text("please run /rework DRAFT-20260815-000043 shorter")
            .expect("slash present")
            .expect("valid rework");
        match r {
            OperatorCommand::Rework {
                draft_id,
                instructions,
            } => {
                assert_eq!(draft_id, "DRAFT-20260815-000043");
                assert!(instructions.to_ascii_lowercase().contains("shorter"));
            }
            other => panic!("expected Rework, got {other:?}"),
        }
    }

    #[test]
    fn inline_slash_bare_command() {
        let r = parse_inline_slash_text("/propose_draft DIGEST-20260812-000001, 1,3")
            .expect("slash present")
            .expect("valid");
        match r {
            OperatorCommand::ProposeDraft { digest_id, indices } => {
                assert_eq!(digest_id.as_deref(), Some("DIGEST-20260812-000001"));
                assert_eq!(indices, vec![1, 3]);
            }
            other => panic!("expected ProposeDraft, got {other:?}"),
        }
    }

    #[test]
    fn inline_slash_absent_when_only_draft_id() {
        assert!(parse_inline_slash_text(
            "this is a test of freeform, what is current situation on DRAFT-20260815-000043"
        )
        .is_none());
    }

    #[test]
    fn inline_slash_skips_https_url() {
        assert!(
            parse_inline_slash_text("see https://example.com/propose_draft then chat").is_none()
        );
    }

    #[test]
    fn inline_slash_unknown_token_is_not_a_command() {
        assert!(parse_inline_slash_text("Use /not_a_real_itcy_cmd foo").is_none());
        assert_eq!(
            classify_channel_text("Use /not_a_real_itcy_cmd foo"),
            ChannelTextKind::Freeform
        );
    }

    #[test]
    fn known_slash_catalog_is_recognized_by_parser() {
        for name in KNOWN_SLASH_COMMANDS {
            assert!(
                is_known_slash_command(name),
                "catalog entry missing from is_known: {name}"
            );
            let err_or_ok = parse_slash_command(name, "");
            match err_or_ok {
                Ok(_) => {}
                Err(e) => {
                    assert!(
                        !e.contains("unknown slash"),
                        "catalog name /{name} must not be unknown: {e}"
                    );
                }
            }
        }
    }

    #[test]
    fn known_slash_skips_unknown_then_finds_real_cmd() {
        let r = parse_inline_slash_text("ignore /fake_cmd then /list")
            .expect("known /list present")
            .expect("list ok");
        assert_eq!(r, OperatorCommand::List);
    }

    #[test]
    fn classify_channel_text_routes_table() {
        assert!(matches!(
            classify_channel_text("help"),
            ChannelTextKind::TextCommand(OperatorCommand::Help)
        ));
        assert!(matches!(
            classify_channel_text("status_itcy"),
            ChannelTextKind::TextCommand(OperatorCommand::Status)
        ));

        match classify_channel_text("Use /propose_draft DIGEST-20260815-000012, 11") {
            ChannelTextKind::InlineSlash(Ok(OperatorCommand::ProposeDraft {
                digest_id,
                indices,
            })) => {
                assert_eq!(digest_id.as_deref(), Some("DIGEST-20260815-000012"));
                assert_eq!(indices, vec![11]);
            }
            other => panic!("expected inline ProposeDraft, got {other:?}"),
        }

        match classify_channel_text("hey bot /list please") {
            ChannelTextKind::InlineSlash(Ok(OperatorCommand::List)) => {}
            other => panic!("expected inline List, got {other:?}"),
        }

        match classify_channel_text("Use /propose_draft") {
            ChannelTextKind::InlineSlash(Ok(OperatorCommand::ProposeDraft {
                digest_id: None,
                indices,
            })) if indices.is_empty() => {}
            other => panic!("expected corpus ProposeDraft, got {other:?}"),
        }

        match classify_channel_text("Use /not_a_real_itcy_cmd") {
            ChannelTextKind::Freeform => {}
            other => panic!("unknown /token must be freeform, got {other:?}"),
        }

        match classify_channel_text("Use /status") {
            ChannelTextKind::InlineSlash(Err(e)) => {
                assert!(e.contains("reserved") || e.contains("status_itcy"), "{e}");
            }
            other => panic!("expected reserved /status inline Err, got {other:?}"),
        }

        assert_eq!(
            classify_channel_text(
                "this is a test of freeform, what is current situation on DRAFT-20260815-000043"
            ),
            ChannelTextKind::Freeform
        );
        assert_eq!(
            classify_channel_text("see https://example.com/path then chat"),
            ChannelTextKind::Freeform
        );
        assert_eq!(
            classify_channel_text("tell me a joke"),
            ChannelTextKind::Freeform
        );
    }

    #[test]
    fn classify_prefers_first_slash_cmd() {
        match classify_channel_text("/list then ignore /delete TWEET-20990101-000001") {
            ChannelTextKind::InlineSlash(Ok(OperatorCommand::List)) => {}
            other => panic!("first /list must win, got {other:?}"),
        }
    }

    #[test]
    fn classify_mention_then_slash() {
        match classify_channel_text("<@U123> Use /status_itcy") {
            ChannelTextKind::InlineSlash(Ok(OperatorCommand::Status)) => {}
            other => panic!("expected inline status after mention, got {other:?}"),
        }
    }

    #[test]
    fn draft_about_splits_subject_and_instructions() {
        let (s, i) = parse_draft_about_args(
            "rtk-ai labs new CEO, try to find a relevent news article and comment about rtk-ai development over year",
        );
        assert!(s.to_ascii_lowercase().contains("rtk-ai labs"));
        assert!(i.to_ascii_lowercase().contains("try to find"));
        let cmd = parse_slash_command("/draft_about", &format!("{s}, {i}")).unwrap();
        match cmd {
            OperatorCommand::DraftAbout {
                subject,
                instructions,
            } => {
                assert_eq!(subject, s);
                assert_eq!(instructions, i);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn status_slash_is_itcy_only() {
        assert!(parse_slash_command("/status", "").is_err());
        assert_eq!(
            parse_slash_command("/status_itcy", "").unwrap(),
            OperatorCommand::Status
        );
    }

    #[test]
    fn parses_slash_workflows() {
        assert_eq!(
            parse_slash_command("accept", "DRAFT-20260728-000001").unwrap(),
            OperatorCommand::Accept {
                draft_id: "DRAFT-20260728-000001".into()
            }
        );
        assert_eq!(
            parse_slash_command("retry_bat", "DRAFT-20260803-000028").unwrap(),
            OperatorCommand::RetryBat {
                draft_id: "DRAFT-20260803-000028".into()
            }
        );
        let r = parse_slash_command("/rework", "DRAFT-20260728-000001 make shorter").unwrap();
        match r {
            OperatorCommand::Rework {
                draft_id,
                instructions,
            } => {
                assert_eq!(draft_id, "DRAFT-20260728-000001");
                assert_eq!(instructions, "make shorter");
            }
            other => panic!("{other:?}"),
        }
        let refresh = parse_slash_command("/rework", "CREPLY-20260902-000006").unwrap();
        match refresh {
            OperatorCommand::Rework {
                draft_id,
                instructions,
            } => {
                assert_eq!(draft_id, "CREPLY-20260902-000006");
                assert!(instructions.is_empty());
            }
            other => panic!("{other:?}"),
        }
        let u = parse_slash_command("change_url", "DRAFT-20260728-000001 2").unwrap();
        match u {
            OperatorCommand::ChangeUrl { draft_id, choice } => {
                assert_eq!(draft_id, "DRAFT-20260728-000001");
                assert_eq!(choice, "2");
            }
            other => panic!("{other:?}"),
        }
        // Operator paste of "Draft ID: …" from the bot reply (first token was "Draft").
        let pasted = parse_slash_command(
            "/rework",
            "Draft ID: DRAFT-20260801-000025, remove dubbed RufRoot",
        )
        .unwrap();
        match pasted {
            OperatorCommand::Rework {
                draft_id,
                instructions,
            } => {
                assert_eq!(draft_id, "DRAFT-20260801-000025");
                assert_eq!(instructions, "remove dubbed RufRoot");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse_slash_command("accept", "Draft ID: DRAFT-20260801-000025").unwrap(),
            OperatorCommand::Accept {
                draft_id: "DRAFT-20260801-000025".into()
            }
        );
        assert_eq!(
            parse_slash_command("/propose_draft", "").unwrap(),
            OperatorCommand::ProposeDraft {
                digest_id: None,
                indices: vec![],
            }
        );
        assert_eq!(
            parse_slash_command("/propose_draft", "3").unwrap(),
            OperatorCommand::ProposeDraft {
                digest_id: None,
                indices: vec![1, 2, 3],
            }
        );
        assert_eq!(
            parse_slash_command("/propose_draft", "DIGEST-20260812-000001, 1,3").unwrap(),
            OperatorCommand::ProposeDraft {
                digest_id: Some("DIGEST-20260812-000001".into()),
                indices: vec![1, 3],
            }
        );
        assert_eq!(
            parse_slash_command("/daily_digest", "").unwrap(),
            OperatorCommand::DailyDigest
        );
    }

    #[test]
    fn parses_reply_comment_slash_workflows() {
        let c = parse_slash_command(
            "/reply_comment",
            "https://www.linkedin.com/feed/update/urn:li:activity:123/",
        )
        .unwrap();
        match c {
            OperatorCommand::ReplyComment { url } => {
                assert!(url.contains("linkedin.com"));
            }
            other => panic!("{other:?}"),
        }
        let x = parse_slash_command(
            "/reply_comment",
            "https://x.com/grok/status/2092338421779796069",
        )
        .unwrap();
        match x {
            OperatorCommand::ReplyComment { url } => {
                assert!(url.contains("grok/status"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse_slash_command("/accept", "XREPLY-20260825-000001").unwrap(),
            OperatorCommand::Accept {
                draft_id: "XREPLY-20260825-000001".into()
            }
        );
        assert!(
            parse_slash_command("/change_url", "XREPLY-20260825-000001 1")
                .unwrap_err()
                .contains("no cite URL")
        );
        assert!(
            parse_slash_command("/change_url", "CREPLY-20260825-000001 1")
                .unwrap_err()
                .contains("no cite URL")
        );
        assert_eq!(
            parse_slash_command("/accept", "CREPLY-20260825-000001").unwrap(),
            OperatorCommand::Accept {
                draft_id: "CREPLY-20260825-000001".into()
            }
        );
        assert!(matches!(
            parse_slash_command("/rework", "XREPLY-20260825-000001 shorter").unwrap(),
            OperatorCommand::Rework { .. }
        ));
        assert!(matches!(
            parse_slash_command("/show", "CREPLY-20260825-000001").unwrap(),
            OperatorCommand::Show { .. }
        ));
        assert!(matches!(
            parse_slash_command("/delete", "XREPLY-20260825-000001").unwrap(),
            OperatorCommand::Delete { .. }
        ));
    }

    #[test]
    fn old_accept_ship_comment_tweet_reply_commands_unknown() {
        for cmd in [
            "accept_comment_reply",
            "ship_comment_reply",
            "accept_tweet_reply",
            "ship_tweet_reply",
        ] {
            assert!(!KNOWN_SLASH_COMMANDS.contains(&cmd), "{cmd}");
            let err = parse_slash_command(cmd, "https://example.com").unwrap_err();
            assert!(
                err.to_ascii_lowercase().contains("unknown")
                    || err.to_ascii_lowercase().contains("usage")
                    || err.contains("not a"),
                "{cmd}: {err}"
            );
        }
    }

    #[test]
    fn parses_tweet_slash_workflows() {
        assert_eq!(
            parse_slash_command("/tweet_about", "owl merge, keep it short").unwrap(),
            OperatorCommand::TweetAbout {
                subject: "owl merge".into(),
                instructions: "keep it short".into(),
            }
        );
        assert_eq!(
            parse_slash_command("/tweet_farce", "").unwrap(),
            OperatorCommand::TweetFarce {
                theme: String::new(),
            }
        );
        assert_eq!(
            parse_slash_command("/tweet_farce", "Cursor agents vs Mars Wi-Fi").unwrap(),
            OperatorCommand::TweetFarce {
                theme: "Cursor agents vs Mars Wi-Fi".into(),
            }
        );
        assert_eq!(
            parse_slash_command("/propose_tweet", "").unwrap(),
            OperatorCommand::ProposeTweet {
                digest_id: None,
                indices: vec![],
            }
        );
        assert_eq!(
            parse_slash_command("/propose_tweet", "DIGEST-20260812-000001, 2").unwrap(),
            OperatorCommand::ProposeTweet {
                digest_id: Some("DIGEST-20260812-000001".into()),
                indices: vec![2],
            }
        );
        assert_eq!(
            parse_slash_command("/accept", "TWEET-20260813-000001").unwrap(),
            OperatorCommand::Accept {
                draft_id: "TWEET-20260813-000001".into()
            }
        );
        assert_eq!(
            parse_slash_command("/accept", "DRAFT-20260813-000001").unwrap(),
            OperatorCommand::Accept {
                draft_id: "DRAFT-20260813-000001".into()
            }
        );
        assert_eq!(
            parse_slash_command("retry_bat", "TWEET-20260813-000001").unwrap(),
            OperatorCommand::RetryBat {
                draft_id: "TWEET-20260813-000001".into()
            }
        );
        assert_eq!(
            parse_slash_command("retry_bat", "XPOST-20260814-000010").unwrap(),
            OperatorCommand::RetryBat {
                draft_id: "XPOST-20260814-000010".into()
            }
        );
        assert_eq!(
            parse_slash_command("/rework", "TWEET-20260813-000001, keep it shorter").unwrap(),
            OperatorCommand::Rework {
                draft_id: "TWEET-20260813-000001".into(),
                instructions: "keep it shorter".into(),
            }
        );
        assert_eq!(
            parse_slash_command("/change_url", "TWEET-20260813-000001, 2").unwrap(),
            OperatorCommand::ChangeUrl {
                draft_id: "TWEET-20260813-000001".into(),
                choice: "2".into(),
            }
        );
    }

    #[test]
    fn compose_brief_joins_instructions() {
        assert_eq!(
            compose_operator_brief("rtk-ai CEO", "find news article"),
            "rtk-ai CEO, find news article"
        );
        assert_eq!(compose_operator_brief("solo topic", ""), "solo topic");
    }

    #[test]
    fn help_mentions_registered_slash_names() {
        let h = help_text();
        assert!(h.contains("`help`"));
        assert!(h.contains("`status_itcy`"));
        assert!(h.contains("/draft_about"));
        assert!(h.contains("/draft_about_itc"));
        assert!(h.contains("/draft_tweet_about_itc"));
        assert!(h.contains("/reply_comment"));
        assert!(!h.contains("/accept_comment_reply"));
        assert!(!h.contains("/ship_tweet_reply"));
        assert!(h.contains("/propose_draft"));
        assert!(h.contains("/daily_digest"));
        assert!(h.contains("/tweet_about"));
        assert!(h.contains("/tweet_farce"));
        assert!(h.contains("/propose_tweet"));
        assert!(h.contains("/accept"));
        assert!(h.contains("/rework"));
        assert!(h.contains("/change_url"));
        assert!(h.contains("/list"));
        assert!(h.contains("/show"));
        assert!(h.contains("/delete"));
        assert!(h.contains("/enrich"));
        assert!(h.contains("/ingest"));
        assert!(h.contains("/handle_add"));
        assert!(!h.contains("/accept_tweet"));
        assert!(!h.contains("/list_tweets"));
        assert!(!h.contains("/rework_draft"));
        assert!(!h.contains("/change_tweet_url"));
        assert!(!h.contains("/change_draft_url"));
    }

    #[test]
    fn itc_slash_allows_empty_and_comma_args() {
        assert_eq!(
            parse_slash_command("/draft_about_itc", "").unwrap(),
            OperatorCommand::DraftAboutItc {
                subject: String::new(),
                instructions: String::new(),
            }
        );
        assert_eq!(
            parse_slash_command("/draft_about_itc", "tvscreener-rs, keep it short").unwrap(),
            OperatorCommand::DraftAboutItc {
                subject: "tvscreener-rs".into(),
                instructions: "keep it short".into(),
            }
        );
        assert_eq!(
            parse_slash_command("/draft_tweet_about_itc", "").unwrap(),
            OperatorCommand::DraftTweetAboutItc {
                subject: String::new(),
                instructions: String::new(),
            }
        );
        assert_eq!(
            parse_slash_command("/draft_tweet_about_itc", "itcy-tui, funny").unwrap(),
            OperatorCommand::DraftTweetAboutItc {
                subject: "itcy-tui".into(),
                instructions: "funny".into(),
            }
        );
    }

    #[test]
    fn unified_lifecycle_slash_accepts_both_surfaces() {
        assert_eq!(
            parse_slash_command("/rework", "DRAFT-20260813-000038 shorter").unwrap(),
            OperatorCommand::Rework {
                draft_id: "DRAFT-20260813-000038".into(),
                instructions: "shorter".into(),
            }
        );
        assert_eq!(
            parse_slash_command("/rework", "TWEET-20260813-000038, shorter").unwrap(),
            OperatorCommand::Rework {
                draft_id: "TWEET-20260813-000038".into(),
                instructions: "shorter".into(),
            }
        );
        assert_eq!(
            parse_slash_command("/change_url", "TWEET-20260813-000001, 0").unwrap(),
            OperatorCommand::ChangeUrl {
                draft_id: "TWEET-20260813-000001".into(),
                choice: "0".into(),
            }
        );
        assert_eq!(
            parse_slash_command("/accept", "DRAFT-20260813-000038").unwrap(),
            OperatorCommand::Accept {
                draft_id: "DRAFT-20260813-000038".into()
            }
        );
        assert_eq!(
            parse_slash_command("/accept", "TWEET-20260813-000001").unwrap(),
            OperatorCommand::Accept {
                draft_id: "TWEET-20260813-000001".into()
            }
        );
    }

    #[test]
    fn legacy_twin_slash_names_are_unknown() {
        for cmd in [
            "/list_drafts",
            "/list_tweets",
            "/show_draft",
            "/show_tweet",
            "/delete_draft",
            "/delete_tweet",
            "/rework_draft",
            "/rework_tweet",
            "/accept_draft",
            "/accept_tweet",
            "/change_draft_url",
            "/change_tweet_url",
        ] {
            let err = parse_slash_command(cmd, "DRAFT-20260813-000001").unwrap_err();
            assert!(err.contains("unknown slash"), "{cmd}: {err}");
        }
    }

    #[test]
    fn change_url_accepts_clear_zero() {
        assert_eq!(
            parse_slash_command("/change_url", "TWEET-20260813-000001, 0").unwrap(),
            OperatorCommand::ChangeUrl {
                draft_id: "TWEET-20260813-000001".into(),
                choice: "0".into(),
            }
        );
    }

    #[test]
    fn propose_tweet_digest_and_bare_n() {
        assert_eq!(
            parse_slash_command("/propose_tweet", "DIGEST-20260812-000001, 1,3").unwrap(),
            OperatorCommand::ProposeTweet {
                digest_id: Some("DIGEST-20260812-000001".into()),
                indices: vec![1, 3],
            }
        );
        assert_eq!(
            parse_slash_command("/propose_tweet", "2").unwrap(),
            OperatorCommand::ProposeTweet {
                digest_id: None,
                indices: vec![1, 2],
            }
        );
    }

    #[test]
    fn list_show_delete_tweet_slash() {
        assert_eq!(
            parse_slash_command("/list", "").unwrap(),
            OperatorCommand::List
        );
        assert_eq!(
            parse_slash_command("/show", "TWEET-20260814-000008").unwrap(),
            OperatorCommand::Show {
                ids: vec!["TWEET-20260814-000008".into()]
            }
        );
        assert_eq!(
            parse_slash_command("/show", "DRAFT-20260813-000038").unwrap(),
            OperatorCommand::Show {
                ids: vec!["DRAFT-20260813-000038".into()]
            }
        );
        assert_eq!(
            parse_slash_command("/delete", "TWEET-20260814-000008").unwrap(),
            OperatorCommand::Delete {
                ids: vec!["TWEET-20260814-000008".into()]
            }
        );
        assert_eq!(
            parse_slash_command("/delete", "TWEET-20260813-000001, TWEET-20260813-000002").unwrap(),
            OperatorCommand::Delete {
                ids: vec![
                    "TWEET-20260813-000001".into(),
                    "TWEET-20260813-000002".into()
                ]
            }
        );
        assert_eq!(
            parse_slash_command("/show", "`TWEET-20260813-000001`, `TWEET-20260813-000002`")
                .unwrap(),
            OperatorCommand::Show {
                ids: vec![
                    "TWEET-20260813-000001".into(),
                    "TWEET-20260813-000002".into()
                ]
            }
        );
        assert!(parse_slash_command("/delete", "").is_err());
        assert_eq!(
            parse_slash_command("/show", "DRAFT-20260814-000001").unwrap(),
            OperatorCommand::Show {
                ids: vec!["DRAFT-20260814-000001".into()]
            }
        );
        assert_eq!(
            parse_slash_command("/show", "DRAFT-20260814-000001, TWEET-20260814-000008").unwrap(),
            OperatorCommand::Show {
                ids: vec![
                    "DRAFT-20260814-000001".into(),
                    "TWEET-20260814-000008".into()
                ]
            }
        );
        assert_eq!(
            parse_slash_command("/show", "DIGEST-20260815-000013").unwrap(),
            OperatorCommand::Show {
                ids: vec!["DIGEST-20260815-000013".into()]
            }
        );
        assert_eq!(
            parse_slash_command("/show", "`DIGEST-20260815-000013`").unwrap(),
            OperatorCommand::Show {
                ids: vec!["DIGEST-20260815-000013".into()]
            }
        );
        assert_eq!(
            parse_slash_command("/delete", "TWEET-20260813-000001, DRAFT-20260814-000001").unwrap(),
            OperatorCommand::Delete {
                ids: vec![
                    "TWEET-20260813-000001".into(),
                    "DRAFT-20260814-000001".into()
                ]
            }
        );
        assert_eq!(
            parse_slash_command("/delete", "DRAFT-20260814-000001").unwrap(),
            OperatorCommand::Delete {
                ids: vec!["DRAFT-20260814-000001".into()]
            }
        );
    }

    #[test]
    fn delete_tweet_ack_lists_comma_ids() {
        let ack = command_ack_text(&OperatorCommand::Delete {
            ids: vec![
                "TWEET-20990101-000001".into(),
                "TWEET-20990101-000002".into(),
            ],
        });
        assert_eq!(
            ack,
            "Received /delete TWEET-20990101-000001, TWEET-20990101-000002"
        );
    }

    #[test]
    fn enrich_and_ingest_slash_parsing() {
        let lk = "https://www.linkedin.com/posts/gregoryroussac_test-activity-1";
        assert!(matches!(
            parse_slash_command("/enrich", lk).unwrap(),
            OperatorCommand::Enrich { url } if url == lk
        ));
        assert!(parse_slash_command("/enrich", "https://example.com/a").is_err());
        let pub_url = "https://www.implicator.ai/some-article";
        assert!(matches!(
            parse_slash_command("/ingest", pub_url).unwrap(),
            OperatorCommand::Ingest { url } if url == pub_url
        ));
        assert!(parse_slash_command("/ingest", lk).is_err());
        let pulse = "https://www.linkedin.com/pulse/when-you-start-speaking-emojis-engage-your-consumers-adrien-lepert/?trackingId=abc";
        let expected =
            "https://www.linkedin.com/pulse/when-you-start-speaking-emojis-engage-your-consumers-adrien-lepert";
        assert!(matches!(
            parse_slash_command("/ingest", pulse).unwrap(),
            OperatorCommand::Ingest { url } if url == expected
        ));
        assert!(parse_slash_command("/enrich", pulse).is_err());
    }

    #[test]
    fn ack_only_for_workflow_commands() {
        assert!(!command_needs_ack(&OperatorCommand::Help));
        assert!(!command_needs_ack(&OperatorCommand::Status));
        assert!(slash_immediate_ack(&OperatorCommand::Help).is_none());
        assert!(slash_immediate_ack(&OperatorCommand::Status).is_none());
        assert!(!command_needs_ack(&OperatorCommand::List));
        assert!(!command_needs_ack(&OperatorCommand::Show {
            ids: vec!["TWEET-20990101-000001".into()]
        }));
        assert!(!command_needs_ack(&OperatorCommand::List));
        assert!(!command_needs_ack(&OperatorCommand::Show {
            ids: vec!["DRAFT-20990101-000001".into()]
        }));
        assert!(command_needs_ack(&OperatorCommand::Show {
            ids: vec!["DIGEST-20990101-000001".into()]
        }));
        assert!(command_needs_ack(&OperatorCommand::Show {
            ids: vec![
                "DRAFT-20990101-000001".into(),
                "DIGEST-20990101-000001".into(),
            ]
        }));
        assert!(command_needs_ack(&OperatorCommand::Delete {
            ids: vec!["TWEET-20990101-000001".into()]
        }));
        assert!(command_needs_ack(&OperatorCommand::Delete {
            ids: vec!["DRAFT-20990101-000001".into()]
        }));
        assert!(command_needs_ack(&OperatorCommand::Ingest {
            url: "https://example.com/a".into()
        }));
        let ack = command_ack_text(&OperatorCommand::Enrich {
            url: "https://www.linkedin.com/posts/gregoryroussac_x".into(),
        });
        assert!(ack.contains("Received /enrich"));
        assert!(ack.contains("https://www.linkedin.com/posts/gregoryroussac_x"));
        assert!(!ack.contains('`'), "no pink code spans in ack: {ack}");
        assert!(!ack.contains(" | "), "no pipe join in ack: {ack}");
        assert!(!ack.to_ascii_lowercase().contains("30-90"));
        assert!(command_ack_text(&OperatorCommand::Help).is_empty());
    }

    #[test]
    fn immediate_ack_covers_every_workflow_command() {
        let cases: Vec<(OperatorCommand, &str)> = vec![
            (
                OperatorCommand::DraftAbout {
                    subject: "rtk-ai labs new CEO".into(),
                    instructions: "find news".into(),
                },
                "Received /draft_about rtk-ai labs new CEO, find news",
            ),
            (
                OperatorCommand::Accept {
                    draft_id: "DRAFT-20990101-000001".into(),
                },
                "Received /accept DRAFT-20990101-000001",
            ),
            (
                OperatorCommand::RetryBat {
                    draft_id: "DRAFT-20990101-000001".into(),
                },
                "Received /retry_bat DRAFT-20990101-000001",
            ),
            (
                OperatorCommand::Rework {
                    draft_id: "DRAFT-20990101-000001".into(),
                    instructions: "shorter".into(),
                },
                "Received /rework DRAFT-20990101-000001, shorter",
            ),
            (
                OperatorCommand::ChangeUrl {
                    draft_id: "DRAFT-20990101-000001".into(),
                    choice: "2".into(),
                },
                "Received /change_url DRAFT-20990101-000001, 2",
            ),
            (
                OperatorCommand::ProposeDraft {
                    digest_id: None,
                    indices: vec![],
                },
                "Received /propose_draft",
            ),
            (
                OperatorCommand::ProposeDraft {
                    digest_id: Some("DIGEST-20990101-000001".into()),
                    indices: vec![1],
                },
                "Received /propose_draft DIGEST-20990101-000001, 1",
            ),
            (OperatorCommand::DailyDigest, "Received /daily_digest"),
            (
                OperatorCommand::ReplyComment {
                    url: "https://x.com/grok/status/2092338421779796069".into(),
                },
                "Received /reply_comment https://x.com/grok/status/2092338421779796069",
            ),
            (
                OperatorCommand::Enrich {
                    url: "https://www.linkedin.com/posts/gregoryroussac_x".into(),
                },
                "Received /enrich https://www.linkedin.com/posts/gregoryroussac_x",
            ),
            (
                OperatorCommand::Ingest {
                    url: "https://www.implicator.ai/a".into(),
                },
                "Received /ingest https://www.implicator.ai/a",
            ),
        ];
        assert_acks(&cases);
    }

    #[test]
    fn immediate_ack_covers_tweet_workflow_commands() {
        let cases: Vec<(OperatorCommand, &str)> = vec![
            (
                OperatorCommand::TweetAbout {
                    subject: "owl merge".into(),
                    instructions: "short".into(),
                },
                "Received /tweet_about owl merge, short",
            ),
            (
                OperatorCommand::DraftAboutItc {
                    subject: String::new(),
                    instructions: String::new(),
                },
                "Received /draft_about_itc",
            ),
            (
                OperatorCommand::DraftAboutItc {
                    subject: "itcy-tui".into(),
                    instructions: "short".into(),
                },
                "Received /draft_about_itc itcy-tui, short",
            ),
            (
                OperatorCommand::DraftTweetAboutItc {
                    subject: String::new(),
                    instructions: String::new(),
                },
                "Received /draft_tweet_about_itc",
            ),
            (
                OperatorCommand::DraftTweetAboutItc {
                    subject: "tvscreener-rs".into(),
                    instructions: String::new(),
                },
                "Received /draft_tweet_about_itc tvscreener-rs",
            ),
            (
                OperatorCommand::ProposeTweet {
                    digest_id: None,
                    indices: vec![],
                },
                "Received /propose_tweet",
            ),
            (
                OperatorCommand::ProposeTweet {
                    digest_id: Some("DIGEST-20990101-000001".into()),
                    indices: vec![2],
                },
                "Received /propose_tweet DIGEST-20990101-000001, 2",
            ),
            (
                OperatorCommand::Rework {
                    draft_id: "TWEET-20990101-000001".into(),
                    instructions: "shorter".into(),
                },
                "Received /rework TWEET-20990101-000001, shorter",
            ),
            (
                OperatorCommand::ChangeUrl {
                    draft_id: "TWEET-20990101-000001".into(),
                    choice: "0".into(),
                },
                "Received /change_url TWEET-20990101-000001, 0",
            ),
            (
                OperatorCommand::Accept {
                    draft_id: "TWEET-20990101-000001".into(),
                },
                "Received /accept TWEET-20990101-000001",
            ),
            (
                OperatorCommand::Delete {
                    ids: vec!["TWEET-20990101-000001".into()],
                },
                "Received /delete TWEET-20990101-000001",
            ),
            (
                OperatorCommand::Delete {
                    ids: vec!["DRAFT-20990101-000001".into()],
                },
                "Received /delete DRAFT-20990101-000001",
            ),
            (OperatorCommand::DailyDigest, "Received /daily_digest"),
        ];
        assert_acks(&cases);
        for (cmd, _) in &cases {
            let ack = slash_immediate_ack(cmd).unwrap();
            assert!(
                !ack.contains("top Interchouette"),
                "no top-project fluff: {ack}"
            );
        }
    }

    #[test]
    fn immediate_ack_covers_tweet_farce() {
        let cases: Vec<(OperatorCommand, &str)> = vec![
            (
                OperatorCommand::TweetFarce {
                    theme: String::new(),
                },
                "Received /tweet_farce",
            ),
            (
                OperatorCommand::TweetFarce {
                    theme: "Mars Wi-Fi".into(),
                },
                "Received /tweet_farce Mars Wi-Fi",
            ),
        ];
        assert_acks(&cases);
    }

    fn assert_acks(cases: &[(OperatorCommand, &str)]) {
        for (cmd, exact) in cases {
            let ack = slash_immediate_ack(cmd).expect("workflow commands need ack");
            assert_eq!(ack, *exact, "ack for {cmd:?}");
            assert!(
                !ack.to_ascii_lowercase().contains("several minutes"),
                "no ETA fluff: {ack}"
            );
            assert!(!ack.contains('…'), "no ellipsis fluff in ack: {ack}");
            assert!(!ack.contains('`'), "no code spans in ack: {ack}");
            assert!(!ack.contains(" | "), "no pipe join in ack: {ack}");
        }
    }

    #[test]
    fn regression_channel_post_sequence_puts_ack_before_reply() {
        let cmd = OperatorCommand::DraftAbout {
            subject: "rtk-ai labs new CEO".into(),
            instructions: "find news".into(),
        };
        let ack = slash_immediate_ack(&cmd).expect("ack");
        let posts = slash_channel_post_sequence(Some(ack.clone()), "FINAL DRAFT BODY".into());
        assert_eq!(posts.len(), 2, "ack + reply");
        assert_eq!(
            posts[0], ack,
            "ack must be first (Socket posts before work)"
        );
        assert_eq!(posts[1], "FINAL DRAFT BODY");
        assert!(
            posts[0].contains("Received /draft_about"),
            "first post is the receipt, not the draft"
        );
        assert!(
            !posts[0].contains("FINAL DRAFT BODY"),
            "ack must not include the final reply"
        );
    }

    #[test]
    fn help_and_status_channel_sequence_is_reply_only() {
        for cmd in [OperatorCommand::Help, OperatorCommand::Status] {
            assert!(slash_immediate_ack(&cmd).is_none());
            let posts = slash_channel_post_sequence(None, "status or help body".into());
            assert_eq!(posts, vec!["status or help body".to_string()]);
        }
    }

    #[test]
    fn draft_about_parse_yields_ack_with_subject_and_instructions() {
        let cmd = parse_slash_command(
            "/draft_about",
            "rtk-ai labs new CEO, try to find a relevent news article",
        )
        .unwrap();
        let ack = slash_immediate_ack(&cmd).unwrap();
        assert!(ack.contains("rtk-ai labs new CEO"));
        assert!(ack.contains("try to find a relevent news article"));
        assert!(
            ack.contains(", "),
            "subject and instructions joined with comma"
        );
        assert!(!ack.contains('`'), "no code spans in ack: {ack}");
        assert!(!ack.contains(" | "), "no pipe join in ack: {ack}");
        assert!(!ack.to_ascii_lowercase().contains("several minutes"));
        assert!(!ack.to_ascii_lowercase().contains("running load"));
    }

    #[test]
    fn slash_reply_headline_is_first_line_only() {
        let reply = "*Enrich complete*\n\
• source `#9205` · via **Tor**\n\
• subject: `today learned that git checkout`\n\
• preview: long body…";
        assert_eq!(slash_reply_headline(reply), "Enrich complete");
    }
}

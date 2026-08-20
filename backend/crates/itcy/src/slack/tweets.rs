// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Slack replies for the tweet BAT loop (`/tweet_about` twin of `/draft_about`).

use crate::bat::store::{
    status, stored_building_stub, stored_from_payload, DraftPayload, DraftStore,
};
use crate::bat::submit::{accept_tweet, ensure_open_for_edit};
use crate::slack::commands::compose_operator_brief;
use crate::slack::handler::SlackRuntime;
use crate::sources::draft_url::extract_in_post_url;
use crate::sources::rag::RagError;
use crate::sources::rework::rework_stored_tweet;
use crate::sources::tweet::build_grounded_tweet;
use crate::sources::tweet_farce::build_farce_tweet;
use crate::sources::tweet_footer::apply_change_tweet_url;
use tracing::{error, info, warn};

fn open_tweet_next(id: &str) -> String {
    format!("/rework {id}\n\n/change_url {id} 1\n\n/accept {id}")
}

impl SlackRuntime {
    pub(crate) async fn tweet_farce_reply(&self, theme: &str) -> String {
        let theme = theme.trim();
        let subject = if theme.is_empty() { "farce" } else { theme };
        let tweet_id = crate::sources::tweet_footer::next_tweet_id(&self.config.state_db_path)
            .unwrap_or_else(|e| {
                warn!(error = %e, "slack: tweet id allocate failed; using fallback");
                format!("TWEET-{}-UNKNOWN", chrono::Local::now().format("%Y%m%d"))
            });
        if let Err(e) = DraftStore::open(&self.config.state_db_path)
            .and_then(|s| s.upsert(&stored_building_stub(&tweet_id, subject)))
        {
            warn!(error = %e, tweet_id = %tweet_id, "slack: farce building stub persist failed");
        }
        match self.tools.begin_research_session(subject, &tweet_id).await {
            Ok(dir) => {
                crate::logging::append_session_log_note(&format!(
                    "slack: /tweet_farce\ntweet_id: {tweet_id}\ntheme: {theme}\nsession: {}\n",
                    dir.display()
                ));
                info!(
                    dir = %dir.display(),
                    tweet_id = %tweet_id,
                    "slack: farce tweet session"
                );
            }
            Err(e) => warn!(error = %e, "slack: could not start farce session log"),
        }
        match build_farce_tweet(
            &self.llm,
            &self.config.state_db_path,
            theme,
            Some(self.tools.as_ref()),
        )
        .await
        {
            Ok(draft) => {
                if let Err(e) = self.persist_grounded_draft(&draft) {
                    error!(error = %e, "slack farce tweet store failed");
                }
                format!(
                    "{body}\n\n\
Saved as open tweet. Ref `{id}`.\n\n\
Next:\n\n\
{next}",
                    body = draft.body,
                    id = draft.draft_id,
                    next = open_tweet_next(&draft.draft_id)
                )
            }
            Err(RagError::FarceMissingMentions) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&tweet_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                format!(
                    "ITCy farce writer missed @grok / @cursor_ai / @elonmusk. Tweet `{tweet_id}` marked failed. Try `/tweet_farce` again."
                )
            }
            Err(RagError::NotATweet) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&tweet_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                format!(
                    "ITCy farce writer dumped an essay instead of a tweet. Tweet `{tweet_id}` marked failed. Try `/tweet_farce` again."
                )
            }
            Err(e) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&tweet_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                error!(error = %e, "slack farce tweet failed");
                format!(
                    "ITCy could not build a farce tweet ({e}). Tweet `{tweet_id}` marked failed."
                )
            }
        }
    }

    pub(crate) async fn draft_tweet_about_itc_reply(
        &self,
        topic: &str,
        instructions: &str,
    ) -> String {
        use crate::sources::itc_digest::{build_itc_research_pack, default_itc_subject};
        use crate::sources::tweet::build_grounded_tweet_from_pack;

        let subject = if topic.trim().is_empty() {
            match default_itc_subject().await {
                Ok(s) => s,
                Err(e) => return format!("`/draft_tweet_about_itc` failed: {e}"),
            }
        } else {
            topic.trim().to_string()
        };
        let operator_brief = compose_operator_brief(&subject, instructions);
        let (pack, urls) = match build_itc_research_pack(&operator_brief).await {
            Ok(v) => v,
            Err(e) => return format!("`/draft_tweet_about_itc` failed: {e}"),
        };

        let tweet_id = crate::sources::tweet_footer::next_tweet_id(&self.config.state_db_path)
            .unwrap_or_else(|e| {
                warn!(error = %e, "slack: tweet id allocate failed; using fallback");
                format!("TWEET-{}-UNKNOWN", chrono::Local::now().format("%Y%m%d"))
            });
        if let Err(e) = DraftStore::open(&self.config.state_db_path)
            .and_then(|s| s.upsert(&stored_building_stub(&tweet_id, subject.trim())))
        {
            warn!(error = %e, tweet_id = %tweet_id, "slack: tweet building stub persist failed");
        }
        match self
            .tools
            .begin_research_session(&operator_brief, &tweet_id)
            .await
        {
            Ok(dir) => {
                crate::logging::append_session_log_note(&format!(
                    "slack: /draft_tweet_about_itc\ntweet_id: {tweet_id}\ntopic: {subject}\ninstructions: {instructions}\nsession: {}\n",
                    dir.display()
                ));
                info!(
                    dir = %dir.display(),
                    tweet_id = %tweet_id,
                    "slack: itc tweet research session"
                );
            }
            Err(e) => warn!(error = %e, "slack: could not start tweet research session log"),
        }
        match build_grounded_tweet_from_pack(
            &self.llm,
            &self.config.state_db_path,
            &operator_brief,
            &pack,
            &urls,
            Some(self.tools.as_ref()),
        )
        .await
        {
            Ok(mut draft) => {
                draft.subject = subject;
                if let Err(e) = self.persist_grounded_draft(&draft) {
                    error!(error = %e, "slack tweet store failed");
                }
                format!(
                    "{body}\n\n\
Saved as open tweet. Ref `{id}`.\n\n\
Next:\n\n\
{next}",
                    body = draft.body,
                    id = draft.draft_id,
                    next = open_tweet_next(&draft.draft_id)
                )
            }
            Err(e) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&tweet_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                error!(error = %e, "slack itc grounded tweet failed");
                format!(
                    "ITCy could not build an Interchouette tweet ({e}). Tweet `{tweet_id}` marked failed."
                )
            }
        }
    }

    pub(crate) async fn tweet_about_itcy_reply(&self, instructions: &str) -> String {
        use crate::sources::self_intro::build_itcy_self_tweet;

        let subject = "ITCy self-introduction";
        let tweet_id = crate::sources::tweet_footer::next_tweet_id(&self.config.state_db_path)
            .unwrap_or_else(|e| {
                warn!(error = %e, "slack: tweet id allocate failed; using fallback");
                format!("TWEET-{}-UNKNOWN", chrono::Local::now().format("%Y%m%d"))
            });
        if let Err(e) = DraftStore::open(&self.config.state_db_path)
            .and_then(|s| s.upsert(&stored_building_stub(&tweet_id, subject)))
        {
            warn!(error = %e, tweet_id = %tweet_id, "slack: self-intro tweet building stub persist failed");
        }
        match self.tools.begin_research_session(subject, &tweet_id).await {
            Ok(dir) => {
                crate::logging::append_session_log_note(&format!(
                    "slack: /tweet_about_itcy\ntweet_id: {tweet_id}\ninstructions: {instructions}\nsession: {}\n",
                    dir.display()
                ));
                info!(
                    dir = %dir.display(),
                    tweet_id = %tweet_id,
                    "slack: itcy self-intro tweet session"
                );
            }
            Err(e) => warn!(error = %e, "slack: could not start self-intro tweet session log"),
        }
        match build_itcy_self_tweet(
            &self.llm,
            &self.config.state_db_path,
            instructions,
            Some(self.tools.as_ref()),
        )
        .await
        {
            Ok(mut draft) => {
                draft.subject = subject.to_string();
                if let Err(e) = self.persist_grounded_draft(&draft) {
                    error!(error = %e, "slack self-intro tweet store failed");
                }
                format!(
                    "{body}\n\n\
Saved as open tweet. Ref `{id}`.\n\n\
Next:\n\n\
{next}",
                    body = draft.body,
                    id = draft.draft_id,
                    next = open_tweet_next(&draft.draft_id)
                )
            }
            Err(RagError::NotATweet) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&tweet_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                format!(
                    "ITCy self-intro writer dumped an essay instead of a tweet. Tweet `{tweet_id}` marked failed. Try `/tweet_about_itcy` again."
                )
            }
            Err(e) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&tweet_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                error!(error = %e, "slack itcy self-intro tweet failed");
                format!(
                    "ITCy could not build a self-introduction tweet ({e}). Tweet `{tweet_id}` marked failed."
                )
            }
        }
    }

    pub(crate) async fn tweet_reply(&self, topic: &str, instructions: &str) -> String {
        let operator_brief = compose_operator_brief(topic, instructions);
        let tweet_id = crate::sources::tweet_footer::next_tweet_id(&self.config.state_db_path)
            .unwrap_or_else(|e| {
                warn!(error = %e, "slack: tweet id allocate failed; using fallback");
                format!("TWEET-{}-UNKNOWN", chrono::Local::now().format("%Y%m%d"))
            });
        if let Err(e) = DraftStore::open(&self.config.state_db_path)
            .and_then(|s| s.upsert(&stored_building_stub(&tweet_id, topic.trim())))
        {
            warn!(error = %e, tweet_id = %tweet_id, "slack: tweet building stub persist failed");
        }
        match self
            .tools
            .begin_research_session(&operator_brief, &tweet_id)
            .await
        {
            Ok(dir) => {
                crate::logging::append_session_log_note(&format!(
                    "slack: /tweet_about\ntweet_id: {tweet_id}\ntopic: {topic}\ninstructions: {instructions}\nsession: {}\n",
                    dir.display()
                ));
                info!(
                    dir = %dir.display(),
                    tweet_id = %tweet_id,
                    "slack: tweet research session"
                );
            }
            Err(e) => warn!(error = %e, "slack: could not start tweet research session log"),
        }
        match build_grounded_tweet(
            &self.llm,
            &self.config.state_db_path,
            self.embed.as_ref(),
            &operator_brief,
            Some(self.tools.as_ref()),
        )
        .await
        {
            Ok(mut draft) => {
                draft.subject = topic.trim().to_string();
                if let Err(e) = self.persist_grounded_draft(&draft) {
                    error!(error = %e, "slack tweet store failed");
                }
                format!(
                    "{body}\n\n\
Saved as open tweet. Ref `{id}`.\n\n\
Next:\n\n\
{next}",
                    body = draft.body,
                    id = draft.draft_id,
                    next = open_tweet_next(&draft.draft_id)
                )
            }
            Err(RagError::NoSources(s)) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&tweet_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                format!(
                    "No corpus hits yet for `{s}`, and the model could not ground a tweet.\n\n\
Try /tweet_about again.\n\n\
Tweet `{tweet_id}` marked failed"
                )
            }
            Err(RagError::NotATweet) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&tweet_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                format!(
                    "Tweet writer failed (essay instead of a tweet).\n\n\
Tweet `{tweet_id}` marked failed."
                )
            }
            Err(e) => {
                let _ = DraftStore::open(&self.config.state_db_path)
                    .ok()
                    .and_then(|st| {
                        st.mark_status_from(&tweet_id, status::BUILDING, status::FAILED)
                            .ok()
                    });
                error!(error = %e, "slack grounded tweet failed");
                format!("ITCy could not build a grounded tweet ({e}). Tweet `{tweet_id}` marked failed.")
            }
        }
    }

    pub(crate) async fn propose_tweet_reply(
        &self,
        digest_id: Option<&str>,
        indices: &[i32],
    ) -> String {
        use crate::sources::digest::{get_digest, latest_open_digest, pick_items};
        use std::fmt::Write;
        let db = self.config.state_db_path.as_path();
        let rec = match digest_id {
            Some(id) => match get_digest(db, id) {
                Ok(Some(r)) => r,
                Ok(None) => return format!("No digest `{id}` in runtime.db."),
                Err(e) => return format!("`/propose_tweet` failed: {e}"),
            },
            None => match latest_open_digest(db) {
                Ok(Some(r)) => r,
                Ok(None) => {
                    return "No open digest.\n\nRun /daily_digest first.".into();
                }
                Err(e) => return format!("`/propose_tweet` failed: {e}"),
            },
        };
        let picked = match pick_items(&rec, indices) {
            Ok(p) => p,
            Err(e) => return format!("`/propose_tweet` failed: {e}"),
        };
        let mut out = format!(
            "From `{digest}`: starting {n} tweet(s):\n",
            digest = rec.digest_id,
            n = picked.len()
        );
        for it in picked {
            let subject = it.subject.clone();
            let instructions = it.url.clone().unwrap_or_default();
            let reply = self.tweet_reply(&subject, &instructions).await;
            let _ = write!(out, "\n--- item {} ---\n{reply}\n", it.idx);
        }
        out
    }

    pub(crate) async fn accept_tweet_reply(&self, tweet_id: &str) -> String {
        match accept_tweet(&self.config.state_db_path, tweet_id).await {
            Ok(r) => {
                if let Some(p) = r.promoted {
                    return format!(
                        "Approve was already on GitHub (webhook had been missed): **XPOST published**:\n\
• tweet: `{draft}`\n\
• xpost: `{post}`\n\
• fork PR: #{pr} → merged\n\
• {detail}\n\
Status: **published**.",
                        draft = p.draft_id,
                        post = p.post_id,
                        pr = p.pr_number,
                        detail = p.detail
                    );
                }
                let x_mode = crate::publish::resolve_x_publish_mode("playground")
                    .map_or_else(|_| "playground".into(), |m| m.as_str().to_string());
                let host = if x_mode == "production" {
                    "org Interchouette-ITC"
                } else {
                    "fork Interchouette"
                };
                let action = if r.updated_existing {
                    format!("Tweet PR **updated** (**{x_mode}**, {host}, base `drafts_tweet`)")
                } else {
                    format!("Tweet PR **opened** (**{x_mode}**, {host}, base `drafts_tweet`)")
                };
                format!(
                    "{action}:\n\
• tweet: `{id}`\n\
• branch: `{branch}`\n\
• PR: {url}\n\
Status: **accepted**. Waiting **gRoussac** Approve = BAT → XPOST.\n\n\
If Approve already landed, or ship failed after BAT:\n\n\
/accept {id}\n\n\
/retry_bat {id}",
                    id = r.draft_id,
                    branch = r.branch,
                    url = r.pr_url
                )
            }
            Err(e) => format!("Could not accept Tweet: {e}"),
        }
    }

    pub(crate) async fn rework_tweet_reply(&self, tweet_id: &str, instructions: &str) -> String {
        if tweet_id.starts_with("DRAFT-") {
            return "use `/rework` with a DRAFT- id".into();
        }
        let stored = match ensure_open_for_edit(&self.config.state_db_path, tweet_id) {
            Ok(d) => d,
            Err(e) => return format!("Could not edit tweet: {e}"),
        };
        let store = match DraftStore::open(&self.config.state_db_path) {
            Ok(s) => s,
            Err(e) => return format!("Could not open draft store: {e}"),
        };
        match rework_stored_tweet(&self.llm, &stored, instructions, Some(self.tools.as_ref())).await
        {
            Ok(rew) => {
                let mut row = stored_from_payload(DraftPayload {
                    draft_id: rew.draft_id.clone(),
                    subject: rew.subject.clone(),
                    body: rew.body.clone(),
                    model: rew.model.clone(),
                    tokens_in: rew.tokens_in,
                    tokens_out: rew.tokens_out,
                    sources: rew.sources,
                    link_options: rew.link_options,
                    research_pack: rew.research_pack,
                });
                row.fork_pr_number = stored.fork_pr_number;
                row.fork_pr_url.clone_from(&stored.fork_pr_url);
                row.created_at.clone_from(&stored.created_at);
                if let Err(e) = store.upsert(&row) {
                    return format!(
                        "Rework produced a body but store failed: {e}\n\n{}",
                        rew.body
                    );
                }
                format!(
                    "{body}\n\n\
Reworked tweet `{id}` saved (**open**).\n\n\
Next:\n\n\
{next}",
                    body = rew.body,
                    id = rew.draft_id,
                    next = open_tweet_next(&rew.draft_id)
                )
            }
            Err(e) => format!("Could not rework tweet: {e}"),
        }
    }

    pub(crate) fn change_tweet_url_reply(&self, tweet_id: &str, choice: &str) -> String {
        if tweet_id.starts_with("DRAFT-") {
            return "use `/change_url` with a DRAFT- id".into();
        }
        let mut stored = match ensure_open_for_edit(&self.config.state_db_path, tweet_id) {
            Ok(d) => d,
            Err(e) => return format!("Could not edit tweet: {e}"),
        };
        let store = match DraftStore::open(&self.config.state_db_path) {
            Ok(s) => s,
            Err(e) => return format!("Could not open draft store: {e}"),
        };
        let prior = extract_in_post_url(&stored.body);
        let (body, options) =
            match apply_change_tweet_url(tweet_id, &stored.body, &stored.link_options, choice) {
                Ok(v) => v,
                Err(e) => return e,
            };
        info!(
            tweet_id = %tweet_id,
            choice = %choice,
            from = ?prior,
            "change_tweet_url: applied"
        );
        stored.body = body;
        stored.link_options = options;
        stored.status = status::OPEN.into();
        stored.updated_at = chrono::Local::now().to_rfc3339();
        if let Err(e) = store.upsert(&stored) {
            return format!("Link change failed to save: {e}");
        }
        format!(
            "{body}\n\n\
Next:\n\n\
{next}",
            body = stored.body,
            next = open_tweet_next(tweet_id),
        )
    }
}

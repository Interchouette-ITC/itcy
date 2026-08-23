// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Draft + GitHub BAT (publications PR loop; ship on merge via publish mode).

pub mod github;
pub mod pack;
pub mod store;
pub mod submit;

pub use github::{
    github_token_from_env_or_creds, is_playground_mode, BatGithubConfig, ClosePrOutcome,
    GithubClient, GithubError, OpenedPr, PromoteResult,
};
pub use pack::{
    body_as_post, body_as_xpost, branch_name_for_draft, branch_name_for_tweet,
    draft_id_from_drafts_path, draft_id_from_path, draft_id_to_post_id, draft_paths,
    draft_post_ids, is_draft_body_path, is_tweet_body_path, pack_draft_files, pack_post_meta,
    pack_tweet_files, pack_xpost_meta, post_id_to_draft_id, post_paths, tweet_id_from_path,
    tweet_id_to_xpost_id, tweet_xpost_ids, xpost_id_to_tweet_id, xpost_paths, DraftFiles,
    PostMetaInput, TweetMetaInput, XpostMetaInput,
};
pub use store::{
    status, stored_building_stub, stored_from_payload, DraftPayload, DraftStore, PendingDraft,
    StoredDraft,
};
pub use submit::{
    accept_draft, accept_tweet, ensure_open_for_edit, retry_bat, BatSubmitError, BatSubmitResult,
    RetryBatResult,
};

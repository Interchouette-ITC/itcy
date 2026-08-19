// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Sources ingest, `LinkedIn` export import, subject RAG, and Slack intents.

pub mod digest;
pub mod draft_footer;
pub mod draft_url;
pub mod embed;
pub mod enrich;
pub mod export;
pub mod html;
pub mod ingest;
pub mod intent;
pub mod itc_catalog;
pub mod itc_digest;
pub mod linkedin_extract;
pub mod live_sites;
pub mod portability;
pub mod rag;
pub mod rework;
pub mod scrape_cache;
pub mod self_intro;
pub mod store;
pub mod tor_status;
pub mod tweet;
pub mod tweet_farce;
pub mod tweet_footer;
pub mod tweet_load;
pub mod tweet_thread;
pub mod twitter;
pub mod twitter_queries;
pub mod twitter_vault;
pub mod url_hygiene;

pub use digest::{
    build_daily_digest, digest_slack_messages, digest_slack_post, format_digest_slack,
    format_ship_fail, format_ship_notice, get_digest, latest_open_digest, pick_items,
    shorten_ship_error, DigestError, DigestItem, DigestRecord, DigestSlackPost,
};
pub use embed::{build_embed_client, default_embed_model, EmbedClient, MockEmbedClient};
pub use enrich::{
    default_probe_url, drip_delay, enrich_linkedin_url_at, enrich_one, is_link_stub,
    is_linkedin_enrich_url, is_transient_tor_fetch, prepare_enrich_db, probe_tor_linkedin,
    process_alive, read_enrich_side_signals, tor_newnym, validate_linkedin_enrich_url, EnrichError,
    EnrichManualResult, EnrichManualVia, EnrichSideSignals, EnrichStep, PidLock, TorPageFetcher,
    TorSocksFetcher, DEFAULT_TOR_CONTROL, DEFAULT_TOR_SOCKS,
};
pub use export::{import_linkedin_export, resolve_export_path, ImportStats};
pub use ingest::{
    ingest_url, resolve_public_fetch_cmd, HttpThenPublicPlaywright, IngestFetchPath, IngestReport,
    PageFetcher, MIN_STORE_CHARS, THIN_TRIGGER_CHARS,
};
pub use intent::{detect_intent, Intent};
pub use live_sites::{load_live_sites, LiveSite, LiveSitesError};
pub use portability::{
    import_portability_corpus, resolve_linkedin_access_token, HttpPortabilityClient,
};
pub use rag::{build_grounded_draft, build_grounded_draft_from_pack, GroundedDraft};
pub use scrape_cache::{
    resolve_scrape_cache_path, ScrapeCache, ScrapeCacheError, ScrapePage, DEFAULT_SCRAPE_CACHE_DB,
};
pub use store::{
    ActivityCount, EnrichStatusCounts, InsertSource, SourceDb, SourceListFilter, SourceListItem,
    SourceRecord,
};
pub use tor_status::{
    log_tor_listen_status, probe_tor_listen, run_tor_listen_watch_loop, TorListenStatus,
};
pub use tweet::{build_grounded_tweet, build_grounded_tweet_from_pack};
pub use tweet_farce::{
    build_farce_tweet, ensure_farce_mentions, farce_has_required_mentions, stored_is_farce,
};
pub use tweet_footer::{compose_tweet_message, next_tweet_id};
pub use twitter::{load_twitter_creds, TwitterCreds, TwitterHit, TwitterTool, TwitterToolError};
pub use twitter_queries::{
    load_twitter_query_pool, plan_twitter_searches_from_pool, query_for_log, PlannedSearch,
    TwitterQueriesError, TwitterQuery, TwitterQueryPool, TwitterSearchPlan, MAX_SEARCHES_PER_RUN,
};
pub use twitter_vault::{
    log_twitter_vault_status, probe_twitter_vault, resolve_twitter_gold_dir, TwitterVaultStatus,
};

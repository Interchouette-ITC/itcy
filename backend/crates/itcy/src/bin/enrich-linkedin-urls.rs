// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Standalone Tor `LinkedIn` URL enrich drip (link-only post/repost stubs).
//!
//! Env: `ITCY_STATE_DB`, `ITCY_SCRAPE_CACHE_DB`, `ITCY_TOR_SOCKS`, `ITCY_TOR_CONTROL`,
//! `ITCY_ENRICH_PROBE_URL`, `ITCY_ENRICH_PID`.
//! Args: `--loop` keep dripping; default one claim; `--skip-probe`.
//!
//! `LinkedIn` wall / hard guest chrome: mark source failed + backoff, bump
//! consecutive wall streak, **continue** the drip (do not exit). Fresh Tor
//! circuit via NEWNYM runs immediately before each Tor GET.

use anyhow::Context;
use itcy::logging::init_tracing_stderr;
use itcy::sources::{
    build_embed_client, default_probe_url, drip_delay, enrich_one, prepare_enrich_db,
    probe_tor_linkedin, resolve_scrape_cache_path, EnrichStep, PidLock, ScrapeCache, SourceDb,
    TorSocksFetcher, DEFAULT_SCRAPE_CACHE_DB, DEFAULT_TOR_CONTROL, DEFAULT_TOR_SOCKS,
};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

struct EnrichPaths {
    db_path: String,
    cache_path: PathBuf,
    socks: String,
    control: String,
    pid_path: String,
    streak_path: PathBuf,
}

fn resolve_enrich_paths() -> EnrichPaths {
    let db_path = env::var("ITCY_STATE_DB").unwrap_or_else(|_| {
        itcy::paths::product_join("sql/runtime.db")
            .display()
            .to_string()
    });
    let cache_path = resolve_scrape_cache_path(
        &env::var("ITCY_SCRAPE_CACHE_DB").unwrap_or_else(|_| DEFAULT_SCRAPE_CACHE_DB.into()),
    );
    let socks = env::var("ITCY_TOR_SOCKS").unwrap_or_else(|_| DEFAULT_TOR_SOCKS.into());
    let control = env::var("ITCY_TOR_CONTROL").unwrap_or_else(|_| DEFAULT_TOR_CONTROL.into());
    let pid_path = env::var("ITCY_ENRICH_PID").unwrap_or_else(|_| {
        PathBuf::from(&db_path)
            .parent()
            .map_or_else(
                || PathBuf::from("enrich-linkedin-urls.pid"),
                |p| p.join("enrich-linkedin-urls.pid"),
            )
            .display()
            .to_string()
    });
    let streak_path = PathBuf::from(&db_path).parent().map_or_else(
        || PathBuf::from("enrich-wall-streak.txt"),
        |p| p.join("enrich-wall-streak.txt"),
    );
    EnrichPaths {
        db_path,
        cache_path,
        socks,
        control,
        pid_path,
        streak_path,
    }
}

async fn sleep_drip(after_wall: Option<u32>) {
    let delay = drip_delay();
    if let Some(streak) = after_wall {
        info!(
            secs = delay.as_secs(),
            wall_streak = streak,
            "drip sleep after wall"
        );
    } else {
        info!(secs = delay.as_secs(), "drip sleep");
    }
    tokio::time::sleep(delay).await;
}

fn write_streak(path: &Path, streak: u32, last_id: Option<i64>) {
    let stamp = chrono::Local::now().to_rfc3339();
    let body = last_id.map_or_else(
        || format!("updated_at={stamp}\nwall_streak={streak}\nlast_wall_source_id=\n"),
        |id| format!("updated_at={stamp}\nwall_streak={streak}\nlast_wall_source_id={id}\n"),
    );
    if let Err(e) = fs::write(path, body) {
        warn!(error = %e, path = %path.display(), "failed to write wall streak file");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing_stderr("info,itcy=info");

    let args: Vec<String> = env::args().collect();
    let do_loop = args.iter().any(|a| a == "--loop");
    let skip_probe = args.iter().any(|a| a == "--skip-probe");
    let paths = resolve_enrich_paths();

    let _lock = PidLock::acquire(&paths.pid_path)
        .with_context(|| format!("acquire enrich pid lock {}", paths.pid_path))?;
    let db = SourceDb::open(&paths.db_path)
        .with_context(|| format!("open sources db {}", paths.db_path))?;
    prepare_enrich_db(&db).context("prepare enrich schema")?;
    let cache = ScrapeCache::open(&paths.cache_path)
        .with_context(|| format!("open scrape cache {}", paths.cache_path.display()))?;
    info!(
        sources = %paths.db_path,
        scrape_cache = %paths.cache_path.display(),
        "enrich stores"
    );

    let fetcher =
        TorSocksFetcher::new(&paths.socks).with_context(|| format!("tor socks {}", paths.socks))?;
    if skip_probe {
        info!("tor probe skipped (--skip-probe)");
    } else {
        let probe = default_probe_url();
        info!(%probe, socks = %paths.socks, "tor probe starting");
        probe_tor_linkedin(&fetcher, &probe)
            .await
            .context("tor LinkedIn probe")?;
        info!("tor probe ok");
    }

    let embed = build_embed_client();
    let control_opt = Some(paths.control.as_str());
    let mut wall_streak: u32 = 0;

    loop {
        match enrich_one(&db, &cache, &fetcher, embed.as_ref(), control_opt).await? {
            EnrichStep::Idle => {
                info!("enrich queue empty");
                if !do_loop {
                    break;
                }
                let delay = drip_delay();
                info!(secs = delay.as_secs(), "sleep until next poll");
                tokio::time::sleep(delay).await;
            }
            EnrichStep::Ok { id } => {
                if wall_streak > 0 {
                    info!(
                        previous_wall_streak = wall_streak,
                        id, "wall streak reset (ok)"
                    );
                }
                wall_streak = 0;
                write_streak(&paths.streak_path, wall_streak, None);
                if !do_loop {
                    break;
                }
                sleep_drip(None).await;
            }
            EnrichStep::Failed { .. } => {
                if !do_loop {
                    break;
                }
                sleep_drip(None).await;
            }
            EnrichStep::Skipped { id, reason } => {
                info!(id, reason, "enrich skipped (parked out of drip)");
                if !do_loop {
                    break;
                }
                sleep_drip(None).await;
            }
            EnrichStep::Wall { id, after } => {
                wall_streak = wall_streak.saturating_add(1);
                write_streak(&paths.streak_path, wall_streak, Some(id));
                warn!(
                    id,
                    %after,
                    wall_streak,
                    "linkedin wall - continuing drip (no process stop)"
                );
                if !do_loop {
                    break;
                }
                sleep_drip(Some(wall_streak)).await;
            }
        }
    }
    Ok(())
}

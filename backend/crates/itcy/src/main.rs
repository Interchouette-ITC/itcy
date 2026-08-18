// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! `ITCy` always-on binary entrypoint.

use anyhow::{Context, Result};
use itcy::app;
use itcy::config::Config;
use itcy::health::AppState;
use itcy::hooks::GithubHookState;
use itcy::llm::build_router;
use itcy::logging::init_tracing;
use itcy::slack::{resolve_slack_runtime, SlackRuntime};
use itcy::sources::build_embed_client;
use itcy::sources::embed::EmbedClient;
use itcy::tools::{resolve_playwright_mcp_cmd, ItcyTools};
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Once};
use tokio::net::TcpListener;
use tokio::signal;
use tracing::{info, warn};

static INSTALL_CRYPTO: Once = Once::new();

fn install_crypto_provider() {
    INSTALL_CRYPTO.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn load_config() -> Result<Config> {
    let config_path =
        env::var("ITCY_CONFIG").map_or_else(|_| PathBuf::from("config.toml"), PathBuf::from);

    let mut config = Config::load(&config_path)
        .with_context(|| format!("load config from {}", config_path.display()))?;

    if let Ok(bind) = env::var("ITCY_BIND") {
        if !bind.is_empty() {
            config.server.bind = bind;
        }
    }
    Ok(config)
}

fn ensure_publish_ready(config: &Config) -> Result<()> {
    let mode = itcy::publish::resolve_publish_mode(&config.linkedin.publish_mode)
        .context("resolve LinkedIn publish mode")?;
    itcy::publish::build_publisher(mode)
        .with_context(|| format!("build company-page publisher (mode={})", mode.as_str()))?;
    info!(
        mode = mode.as_str(),
        "publish: company-page publisher ready"
    );

    itcy::publish::PublishAuditStore::open(&config.runtime.state_db_path).with_context(|| {
        format!(
            "open publish audit store at {}",
            config.runtime.state_db_path
        )
    })?;
    info!(
        db = %config.runtime.state_db_path,
        "publish: audit schema ready"
    );

    match itcy::bat::DraftStore::open(&config.runtime.state_db_path) {
        Ok(store) => match store.fail_all_building() {
            Ok(0) => {}
            Ok(n) => info!(count = n, "drafts: marked stale building → failed"),
            Err(e) => warn!(error = %e, "drafts: fail_all_building failed"),
        },
        Err(e) => warn!(error = %e, "drafts: open store at boot failed"),
    }
    Ok(())
}

fn spawn_browse_warmup(tools: &Arc<ItcyTools>) {
    let tools_warmup = Arc::clone(tools);
    tokio::spawn(async move {
        if let Err(e) = tools_warmup.warmup_browse().await {
            warn!(
                error = %e,
                "tools: Chrome warmup failed; will retry on first browse/search"
            );
        }
    });
}

/// When true, skip chat + embed Ollama pin at boot (tests / smoke without a loaded model).
/// Truthy: `1` / `true` / `yes` (same as `FAST_DEV`). Live stack leaves this unset.
fn skip_ollama_warm() -> bool {
    env::var("ITCY_SKIP_OLLAMA_WARM")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

/// Warm Ollama chat + embed models required by live routes. Hard-fails boot on error.
async fn warm_ollama_or_die(
    llm: &Arc<itcy::llm::FailoverRouter>,
    embed: &Arc<dyn EmbedClient>,
) -> Result<()> {
    if skip_ollama_warm() {
        warn!("ollama: boot warm skipped (ITCY_SKIP_OLLAMA_WARM)");
        return Ok(());
    }
    llm.unload_ollama_models()
        .await
        .context("ollama unload before warm failed; refusing to start")?;
    let embed_model = itcy::sources::embed::default_embed_model();
    if embed.provider_id() == "ollama" {
        embed.warm_model(&embed_model).await.with_context(|| {
            format!("ollama embed warm failed for model={embed_model}; refusing to start")
        })?;
        llm.unload_ollama_models()
            .await
            .context("ollama unload after embed warm failed; refusing to start")?;
    }
    llm.warm_ollama_chat_models()
        .await
        .context("ollama chat warm failed; refusing to start")?;
    Ok(())
}

fn start_github_hooks(config: &Config) -> GithubHookState {
    let github_hooks = GithubHookState::from_env().with_ship_context(
        config.runtime.state_db_path.clone(),
        config.linkedin.publish_mode.clone(),
    );
    info!(
        org = %github_hooks.publications_full_name,
        fork = %github_hooks.drafts_full_name,
        "hooks: enabled (POST /hooks/github; org ingress HMAC)"
    );

    let ngrok_hooks = github_hooks.clone();
    tokio::spawn(async move {
        itcy::hooks::ngrok_inspect::run_ngrok_inspect_loop(ngrok_hooks).await;
    });
    info!("hooks: ngrok inspect poll started (delivery warn)");
    github_hooks
}

fn start_slack_operator(
    config: &Config,
    llm: Arc<itcy::llm::FailoverRouter>,
    embed: Arc<dyn EmbedClient>,
    tools: Arc<ItcyTools>,
) -> Option<Arc<SlackRuntime>> {
    let operator = resolve_slack_runtime(config).and_then(|slack_cfg| {
        match SlackRuntime::new(slack_cfg, llm, embed, tools) {
            Ok(runtime) => {
                info!(
                    channel = %runtime.config.channel_id,
                    "slack: runtime enabled"
                );
                let runtime = Arc::new(runtime);
                tokio::spawn(itcy::slack::socket::run_socket_mode_loop(Arc::clone(
                    &runtime,
                )));
                Some(runtime)
            }
            Err(e) => {
                warn!(error = %e, "slack: failed to open memory db; continuing without Slack");
                None
            }
        }
    });
    if operator.is_some() {
        info!("e2e: POST /e2e/message ready (localhost inject; bot WS self-posts are ignored)");
    }
    operator
}

#[tokio::main]
async fn main() -> Result<()> {
    install_crypto_provider();
    let _ = dotenvy::dotenv();
    // Repo-root `.env` wins over stale exports in the long-lived screen shell.
    let _ = dotenvy::from_filename_override("../.env");
    let _ = dotenvy::dotenv_override();
    init_tracing();

    let config = load_config()?;

    let llm = Arc::new(build_router(&config.llm));
    let embed = build_embed_client();
    let tools = Arc::new(ItcyTools::new(
        PathBuf::from(&config.runtime.state_db_path),
        Arc::clone(&embed),
        resolve_playwright_mcp_cmd(),
    ));
    let tools_keepalive = Arc::clone(&tools);
    warm_ollama_or_die(&llm, &embed).await?;
    spawn_browse_warmup(&tools);

    {
        let tor = itcy::sources::probe_tor_listen();
        itcy::sources::log_tor_listen_status(&tor);
        tokio::spawn(itcy::sources::run_tor_listen_watch_loop());
    }
    {
        let tw = itcy::sources::probe_twitter_vault();
        itcy::sources::log_twitter_vault_status(&tw);
    }

    ensure_publish_ready(&config)?;

    let github_hooks = start_github_hooks(&config);
    let operator = start_slack_operator(&config, Arc::clone(&llm), embed, Arc::clone(&tools));
    let http_state = AppState::new(Arc::clone(&llm), github_hooks, operator.clone());

    let listener = TcpListener::bind(&config.server.bind)
        .await
        .with_context(|| format!("bind {}", config.server.bind))?;

    let addr = listener
        .local_addr()
        .context("read listener local address")?;
    info!(
        %addr,
        "itcy listening (GET /health, GET /status, POST /entrypoint/slash, POST /e2e/message, POST /hooks/github)"
    );

    if let Some(runtime) = operator.as_ref() {
        let linkedin_mode =
            itcy::publish::resolve_publish_mode_agile(&config.linkedin.publish_mode).map_or_else(
                |_| config.linkedin.publish_mode.clone(),
                |m| m.as_str().to_string(),
            );
        let x_mode = itcy::publish::resolve_x_publish_mode(&config.x.publish_mode).map_or_else(
            |_| config.x.publish_mode.clone(),
            |m| m.as_str().to_string(),
        );
        let text = itcy::slack::boot_ready_text(&addr.to_string(), &linkedin_mode, &x_mode);
        let token = runtime.config.bot_token.clone();
        let channel = runtime.config.channel_id.clone();
        tokio::spawn(async move {
            if let Err(e) = itcy::slack::api::post_message(&token, &channel, &text).await {
                warn!(error = %e, "slack: boot ready post failed");
            }
        });
    }

    let server = axum::serve(
        listener,
        app(http_state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal());
    server.await.context("HTTP server")?;
    drop(tools_keepalive);
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

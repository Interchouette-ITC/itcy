// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! One-shot curated `LinkedIn` export → sources DB (linkedin-itcy-GR bootstrap).
//!
//! Env: `ITCY_LINKEDIN_EXPORT_DIR`, `ITCY_STATE_DB` (defaults under product root).
//! Args: `--wipe` clears sources+chunks first (keeps Slack memory tables).

use anyhow::{bail, Context, Result};
use itcy::logging::init_tracing_stderr;
use itcy::paths::product_join;
use itcy::sources::{build_embed_client, import_linkedin_export, SourceDb};
use std::env;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing_stderr("info,itcy=info");

    let wipe = env::args().any(|a| a == "--wipe");
    let export = env::var("ITCY_LINKEDIN_EXPORT_DIR")
        .unwrap_or_else(|_| product_join("linkedin-export").display().to_string());
    let db = env::var("ITCY_STATE_DB")
        .unwrap_or_else(|_| product_join("sql/runtime.db").display().to_string());
    let export = PathBuf::from(export);
    let db = PathBuf::from(db);
    if !export.exists() {
        bail!("export path missing: {}", export.display());
    }
    if wipe {
        let store =
            SourceDb::open(&db).with_context(|| format!("open sources db {}", db.display()))?;
        store
            .clear_corpus()
            .with_context(|| format!("clear corpus in {}", db.display()))?;
        eprintln!("cleared sources+chunks in {}", db.display());
    }
    let embed = build_embed_client();
    let stats = import_linkedin_export(&export, &db, embed.as_ref())
        .await
        .with_context(|| {
            format!(
                "import LinkedIn export from {} into {}",
                export.display(),
                db.display()
            )
        })?;
    eprintln!(
        "linkedin-itcy-GR import: inserted={} skipped={} db={}",
        stats.inserted,
        stats.skipped,
        db.display()
    );
    Ok(())
}

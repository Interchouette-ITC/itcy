// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Embed writer prompts at compile time.
//!
//! When operator prompt files exist next to this checkout, copy them into
//! `OUT_DIR`. Otherwise write short stubs so the crate still compiles.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const PROMPT_FILES: &[&str] = &[
    "who_is_who.md",
    "ai_cmo.md",
    "creative_x.md",
    "creative_linkedin.md",
    "form_craft_x.md",
    "form_craft_linkedin.md",
    "load_system.md",
    "draft_system.md",
    "tweet_system.md",
    "tweet_rework_system.md",
    "tweet_farce_system.md",
    "freeform_system.md",
    "load_user.md",
    "draft_user.md",
    "tweet_user.md",
    "tweet_farce_user.md",
    "fallback_commentary.md",
    "draft_rework_user.md",
    "tweet_rework_user.md",
    "tweet_rework_user_tools.md",
    "tweet_rework_user_farce.md",
    "rework_empty_pack.md",
    "tweet_rework_previous_omitted.md",
    "tweet_rework_commentary_exploded.md",
    "tweet_rework_commentary_empty.md",
    "tweet_pack_note_subject_https.md",
    "tweet_pack_note_empty.md",
    "tweet_pack_note_normal.md",
    "draft_pack_note_empty.md",
    "draft_pack_note_normal.md",
    "draft_pack_note_subject_https.md",
    "draft_user_subject_https.md",
    "self_system.md",
    "self_user.md",
    "draft_rework_system.md",
    "comment_reply_system.md",
    "comment_reply_user.md",
    "tweet_reply_system.md",
    "tweet_reply_user.md",
];

fn main() {
    println!("cargo:rustc-check-cfg=cfg(itcy_kitchen_prompts)");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let product_root = manifest
        .ancestors()
        .nth(3)
        .expect("crate lives at backend/crates/itcy")
        .to_path_buf();
    let kitchen = product_root.join(".cursor").join("prompts");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("prompts");
    fs::create_dir_all(&out).expect("create OUT_DIR/prompts");

    println!("cargo:rerun-if-changed={}", kitchen.display());
    watch_kitchen_files(&kitchen);

    let have_kitchen = kitchen.join("who_is_who.md").is_file();
    if have_kitchen {
        println!("cargo:rustc-cfg=itcy_kitchen_prompts");
    }

    for name in PROMPT_FILES {
        let dest = out.join(name);
        let src = kitchen.join(name);
        if src.is_file() {
            fs::copy(&src, &dest).unwrap_or_else(|err| panic!("copy {name}: {err}"));
        } else {
            fs::write(&dest, stub_body(name)).unwrap_or_else(|err| panic!("stub {name}: {err}"));
        }
    }
}

fn watch_kitchen_files(kitchen: &Path) {
    let Ok(entries) = fs::read_dir(kitchen) else {
        return;
    };
    for entry in entries.flatten() {
        println!("cargo:rerun-if-changed={}", entry.path().display());
    }
}

fn stub_body(name: &str) -> &'static str {
    match name {
        "load_user.md" | "rework_empty_pack.md" => "{subject}\n",
        "draft_user.md" | "tweet_user.md" => "{research_pack}\n{pack_note}\n{subject}\n",
        "tweet_farce_user.md" => "{theme}\n",
        "fallback_commentary.md" => "{topic}\n",
        "draft_rework_user.md" => "{instructions}\n{id}\n{subject}\n{pack}\n{body}\n{url_lock}\n",
        "tweet_rework_user.md" => "{instructions}\n{id}\n{subject}\n{commentary}\n{cite}\n",
        "tweet_rework_user_tools.md" => {
            "{instructions}\n{id}\n{subject}\n{commentary}\n{cite}\n{pack}\n"
        }
        "tweet_rework_user_farce.md" => "{instructions}\n{id}\n{subject}\n{commentary}\n",
        "self_user.md" => "{surface}\n{instructions}\n",
        "comment_reply_user.md" => "{parent_post}\n{comment_author}\n{comment_body}\n",
        "tweet_reply_user.md" => "{tweet_author}\n{tweet_body}\n",
        _ => "stub\n",
    }
}

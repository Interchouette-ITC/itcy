// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Bare `/propose_draft` / `/propose_tweet`: resolve a concrete subject from corpus.

use crate::bat::store::DraftStore;
use crate::sources::store::{ChunkRecord, SourceDb};
use crate::sources::url_hygiene::{is_junk_or_search_url, scrub_https_url};
use std::collections::HashSet;
use std::path::Path;

/// `LinkedIn` vs X surface for bare corpus propose instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposeSurface {
    /// Company `LinkedIn` post.
    Draft,
    /// Company X tweet.
    Tweet,
}

const CANDIDATE_LIMIT: u32 = 250;
const USED_SUBJECT_LIMIT: usize = 200;
const MIN_SUBJECT_CHARS: usize = 8;
const MAX_SUBJECT_CHARS: usize = 160;
const MAX_GROUNDING_CHARS: usize = 900;
const MIN_CHUNK_TEXT: usize = 40;

/// Resolve a concrete subject + instructions from corpus (no catalog fallback).
///
/// Skips corpus angles whose subject or topic tokens already appear on an in-flight
/// or shipped `DRAFT-` / `TWEET-` row so bare propose does not re-open the same story.
///
/// # Errors
///
/// Returns an operator-facing message when the corpus has no usable chunks,
/// or every recent angle is already covered by a draft/tweet.
pub fn resolve_corpus_propose_brief(
    db_path: &Path,
    surface: ProposeSurface,
) -> Result<(String, String), String> {
    let db = SourceDb::open(db_path).map_err(|e| format!("Corpus open failed: {e}"))?;
    let chunks = db
        .get_chunk_candidates("", CANDIDATE_LIMIT)
        .map_err(|e| format!("Corpus read failed: {e}"))?;
    if chunks.is_empty() {
        return Err(
            "Corpus is empty (no chunks). Import LinkedIn export or `/ingest` / `/enrich` first. \
Bare `/propose_draft` / `/propose_tweet` need corpus memory."
                .into(),
        );
    }
    let used_subjects = load_used_propose_subjects(db_path);
    let used_topics = load_used_propose_topic_fingerprints(db_path, surface);
    let Some(picked) = pick_corpus_angle(&chunks, &used_subjects, &used_topics) else {
        if used_subjects.is_empty() && used_topics.is_empty() {
            return Err(
                "Corpus has chunks but no usable subject angle. Try `/ingest` a publisher URL or refresh the LinkedIn export."
                    .into(),
            );
        }
        return Err(
            "Every recent corpus angle already has an open, accepted, or published draft/tweet. \
`/ingest` something new, or `/draft_about <topic>` / `/tweet_about <topic>`."
                .into(),
        );
    };
    let subject = picked.subject;
    let grounding = picked.grounding;
    let instructions = match surface {
        ProposeSurface::Draft => format!(
            "Propose one company-page LinkedIn post from corpus memory on this subject. \
Stay on this subject. Do not invent a different story. \
Use corpus_search for voice/history. Use web_search / browse_url only to support this subject \
and find an on-topic publisher cite when useful.\n\n\
Corpus grounding:\n{grounding}"
        ),
        ProposeSurface::Tweet => format!(
            "Propose one company X tweet from corpus memory on this subject. \
Stay on this subject. Do not invent a different story. \
Use web_search / browse_url only to support this subject and find an on-topic publisher cite when useful.\n\n\
Corpus grounding:\n{grounding}"
        ),
    };
    Ok((subject, instructions))
}

fn load_used_propose_subjects(db_path: &Path) -> HashSet<String> {
    let Ok(store) = DraftStore::open(db_path) else {
        return HashSet::new();
    };
    let Ok(subjects) = store.used_propose_subjects(USED_SUBJECT_LIMIT) else {
        return HashSet::new();
    };
    subjects
        .into_iter()
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| !s.is_empty() && s != "what we know")
        .collect()
}

fn load_used_propose_topic_fingerprints(
    db_path: &Path,
    surface: ProposeSurface,
) -> Vec<HashSet<String>> {
    let Ok(store) = DraftStore::open(db_path) else {
        return Vec::new();
    };
    let Ok(rows) = store.used_propose_topic_subjects(USED_SUBJECT_LIMIT) else {
        return Vec::new();
    };
    rows.into_iter()
        .filter(|(draft_id, _)| propose_row_matches_surface(draft_id, surface))
        .map(|(_draft_id, subject)| topic_fingerprint(&subject))
        .filter(|fp| !fp.is_empty())
        .collect()
}

fn propose_row_matches_surface(draft_id: &str, surface: ProposeSurface) -> bool {
    match surface {
        ProposeSurface::Draft => draft_id.starts_with("DRAFT-"),
        ProposeSurface::Tweet => draft_id.starts_with("TWEET-"),
    }
}

fn pick_corpus_angle(
    chunks: &[ChunkRecord],
    used_subjects: &HashSet<String>,
    used_topics: &[HashSet<String>],
) -> Option<PickedAngle> {
    let mut seen: Vec<String> = Vec::new();
    for c in chunks {
        let subject = angle_subject(c)?;
        let key = subject.to_ascii_lowercase();
        if used_subjects.contains(&key) {
            continue;
        }
        if seen.iter().any(|s| s == &key) {
            continue;
        }
        if !chunk_text_usable(&c.text) {
            continue;
        }
        let candidate_fp = topic_fingerprint(&subject);
        if used_topics
            .iter()
            .any(|used| topic_fingerprints_overlap(&candidate_fp, used))
        {
            continue;
        }
        seen.push(key);
        let grounding = clip_chars(c.text.trim(), MAX_GROUNDING_CHARS);
        return Some(PickedAngle { subject, grounding });
    }
    None
}

fn topic_fingerprint(text: &str) -> HashSet<String> {
    let normalized = normalize_ascii_apostrophes(&text.to_ascii_lowercase()).replace('-', " ");
    let words: Vec<&str> = normalized
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let mut out = HashSet::new();
    for w in &words {
        if (w.len() >= 4 && !TOPIC_STOPWORDS.contains(w)) || SHORT_TOPIC_TOKENS.contains(w) {
            out.insert((*w).to_string());
        }
    }
    for pair in words.windows(2) {
        if pair[0] == "jpeg" && pair[1] == "xl" {
            out.insert("jpeg_xl".into());
        }
    }
    out
}

fn topic_fingerprints_overlap(a: &HashSet<String>, b: &HashSet<String>) -> bool {
    let shared: Vec<&String> = a.intersection(b).collect();
    if shared.is_empty() {
        return false;
    }
    if shared.len() >= 3 {
        return true;
    }
    if shared.len() == 2 {
        return shared
            .iter()
            .any(|t| STRONG_TOPIC_SINGLE.contains(&t.as_str()));
    }
    let only = shared[0].as_str();
    STRONG_TOPIC_SINGLE.contains(&only) || only.contains('_')
}

const TOPIC_STOPWORDS: &[&str] = &[
    "about", "after", "also", "been", "from", "have", "into", "just", "more", "news", "that",
    "their", "there", "these", "they", "this", "what", "when", "will", "with", "your",
];

const SHORT_TOPIC_TOKENS: &[&str] = &["aws", "avif", "mcp", "ssl", "tls", "xl"];

const STRONG_TOPIC_SINGLE: &[&str] = &[
    "avif",
    "casper",
    "discord",
    "duckdb",
    "ducklabs",
    "jpeg",
    "jpeg_xl",
    "mozilla",
    "symfony",
    "wireguard",
];

fn normalize_ascii_apostrophes(s: &str) -> String {
    s.replace(['\u{2019}', '\u{2018}'], "'")
}

fn angle_subject(c: &ChunkRecord) -> Option<String> {
    let from_col = normalize_subject_line(&c.subject);
    if let Some(s) = from_col {
        return Some(s);
    }
    c.text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .and_then(normalize_subject_line)
}

fn normalize_subject_line(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    if lower == "what we know"
        || lower == "unknown"
        || lower == "digest item"
        || lower.starts_with("http://")
        || lower.starts_with("https://")
    {
        return None;
    }
    let clipped = clip_chars(t, MAX_SUBJECT_CHARS);
    if clipped.chars().count() < MIN_SUBJECT_CHARS {
        return None;
    }
    Some(clipped)
}

fn chunk_text_usable(text: &str) -> bool {
    text.trim().chars().count() >= MIN_CHUNK_TEXT
}

fn clip_chars(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// Drop pack / SERP URLs that are junk or clearly off-angle entertainment / wrong-industry.
#[must_use]
pub fn filter_pack_urls_for_subject(urls: &[String], _subject: &str) -> Vec<String> {
    urls.iter()
        .filter_map(|u| {
            let scrubbed = scrub_https_url(u);
            if scrubbed.is_empty() || is_junk_or_search_url(&scrubbed) {
                return None;
            }
            if is_off_angle_url(&scrubbed) {
                return None;
            }
            Some(scrubbed)
        })
        .collect()
}

/// True when body shows hard off-angle drift (entertainment / wrong-industry markers).
#[must_use]
pub fn body_abandons_subject(body: &str, _subject: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    OFF_ANGLE_MARKERS
        .iter()
        .any(|m| lower.contains(&m.to_ascii_lowercase()))
}

/// True when body uses banned `LinkedIn` slogan mush (prompt-only is not enough).
#[must_use]
pub fn body_has_slogan_mush(body: &str) -> bool {
    let lower = normalize_ascii_apostrophes(&body.to_ascii_lowercase());
    SLOGAN_MUSH_NEEDLES.iter().any(|n| lower.contains(*n))
}

/// Drop sentences that contain slogan mush; keeps paragraph breaks.
#[must_use]
pub fn strip_slogan_mush_sentences(body: &str) -> String {
    body.split("\n\n")
        .map(strip_slogan_mush_paragraph)
        .filter(|p| !p.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn strip_slogan_mush_paragraph(para: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    let normalized = para.replace('\n', " ");
    for chunk in normalized.split(". ") {
        let s = chunk.trim().trim_end_matches('.');
        if s.is_empty() || body_has_slogan_mush(s) {
            continue;
        }
        kept.push(s.to_string());
    }
    kept.join(". ")
}

const SLOGAN_MUSH_NEEDLES: &[&str] = &[
    "it's not about",
    "it is not about",
    "it's not just",
    "it is not just",
    "that's not just",
    "that is not just",
    "isn't just about",
    "isn't just a",
    "isn't just another",
    "this isn't just",
    "this is not just",
    "not just another tool",
    "it's a shift",
    "this isn't just a shift",
    "broader trend",
    "not just about speed",
    "isn't just a technical",
    "isn't just about code",
    "watching how this change becomes habit",
    "watching how this shift becomes habit",
    "watching how these small changes become habits",
    // Recycled ITCy voice-bank sludge (models clone these under every Rust subject).
    "feels right",
    "careful diffs",
    "honest process",
    "diffs honest",
    "process clear",
    "🦀 energy",
    "less friction",
    "focus on what matters",
    "shape the future of",
    "built for the long haul",
    "writing code that feels right",
];

struct PickedAngle {
    subject: String,
    grounding: String,
}

fn is_off_angle_url(url: &str) -> bool {
    let l = url.to_ascii_lowercase();
    OFF_ANGLE_URL_NEEDLES.iter().any(|n| l.contains(n))
}

const OFF_ANGLE_URL_NEEDLES: &[&str] = &[
    "programme-television.org",
    "nouveautes-tele.com",
    "feminactu.com",
    "ici-tout-commence",
    "ici_tout_commence",
    "feuilleton",
    "spoilers",
    "economictimes.com",
    "fssai",
    "/atta/",
    "melles750.fr",
];

const OFF_ANGLE_MARKERS: &[&str] = &[
    "ici tout commence",
    "garde à vue",
    "garde a vue",
    "feuilleton",
    "spoilers",
    "fssai",
    "atta flour",
    "programme-television",
    "nouveautes-tele",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bat::store::{status, stored_from_payload, DraftPayload, DraftStore};
    use crate::sources::store::InsertSource;
    use tempfile::tempdir;

    fn seed_chunk(db: &SourceDb, subject: &str, text: &str) {
        let id = db
            .insert_source(&InsertSource {
                kind: "personal_feed",
                activity: "post",
                subject,
                title: subject,
                url: None,
                raw_text: text,
                occurred_at: None,
            })
            .expect("insert source")
            .expect("new id");
        db.insert_chunk(id, subject, text, &[0.1_f32, 0.2, 0.3])
            .expect("insert chunk");
    }

    #[test]
    fn resolve_picks_concrete_subject_not_what_we_know() {
        let dir = tempdir().expect("temp");
        let path = dir.path().join("runtime.db");
        let db = SourceDb::open(&path).expect("open");
        seed_chunk(
            &db,
            "WebAssembly Component Model stability",
            "Builders are watching the Component Model move toward 1.0 and WASI P3. \
The shift matters for modular secure systems and independent deploy of components.",
        );
        drop(db);
        let (subject, instructions) =
            resolve_corpus_propose_brief(&path, ProposeSurface::Draft).expect("resolve");
        assert_ne!(subject.to_ascii_lowercase(), "what we know");
        assert!(subject.to_ascii_lowercase().contains("component"));
        assert!(instructions.contains("Corpus grounding:"));
        assert!(instructions.contains("Stay on this subject"));
    }

    #[test]
    fn resolve_skips_subject_already_published() {
        let dir = tempdir().expect("temp");
        let path = dir.path().join("runtime.db");
        let db = SourceDb::open(&path).expect("open");
        seed_chunk(
            &db,
            "JPEG XL Mozilla ship rewrite",
            "Mozilla would not ship JPEG XL until someone rewrote the decoder in Rust. \
Google Research did it and builders care about the codec landing in the browser.",
        );
        seed_chunk(
            &db,
            "ducklabs join aws projects remain open",
            "DuckLabs joining AWS keeps DuckDB open source under the DuckDB Foundation. \
Builders care about stewardship without swallowing the community around the stack.",
        );
        drop(db);

        let store = DraftStore::open(&path).expect("drafts");
        let mut shipped = stored_from_payload(DraftPayload {
            draft_id: "DRAFT-20260827-000117".into(),
            subject: "ducklabs join aws projects remain open".into(),
            body: "body".into(),
            model: "mock".into(),
            tokens_in: 1,
            tokens_out: 1,
            sources: vec!["https://techzine.eu/duckdb".into()],
            link_options: Vec::new(),
            research_pack: String::new(),
        });
        shipped.status = status::PUBLISHED.into();
        store.upsert(&shipped).expect("upsert published");
        drop(store);

        let (subject, _) =
            resolve_corpus_propose_brief(&path, ProposeSurface::Draft).expect("resolve");
        assert!(
            subject.to_ascii_lowercase().contains("jpeg"),
            "expected next unused angle, got {subject}"
        );
        assert!(
            !subject.to_ascii_lowercase().contains("ducklabs"),
            "must not re-propose published subject: {subject}"
        );
    }

    #[test]
    fn resolve_errors_when_all_angles_already_used() {
        let dir = tempdir().expect("temp");
        let path = dir.path().join("runtime.db");
        let db = SourceDb::open(&path).expect("open");
        seed_chunk(
            &db,
            "ducklabs join aws projects remain open",
            "DuckLabs joining AWS keeps DuckDB open source under the DuckDB Foundation. \
Builders care about stewardship without swallowing the community around the stack.",
        );
        drop(db);

        let store = DraftStore::open(&path).expect("drafts");
        store
            .upsert(&stored_from_payload(DraftPayload {
                draft_id: "DRAFT-20260827-000118".into(),
                subject: "ducklabs join aws projects remain open".into(),
                body: "body".into(),
                model: "mock".into(),
                tokens_in: 1,
                tokens_out: 1,
                sources: Vec::new(),
                link_options: Vec::new(),
                research_pack: String::new(),
            }))
            .expect("upsert open");
        drop(store);

        let err = resolve_corpus_propose_brief(&path, ProposeSurface::Draft).unwrap_err();
        assert!(
            err.to_ascii_lowercase().contains("already"),
            "expected all-angles-used error, got {err}"
        );
    }

    #[test]
    fn resolve_empty_corpus_errors() {
        let dir = tempdir().expect("temp");
        let path = dir.path().join("empty.db");
        let _db = SourceDb::open(&path).expect("open");
        let err = resolve_corpus_propose_brief(&path, ProposeSurface::Tweet).unwrap_err();
        assert!(err.to_ascii_lowercase().contains("empty"));
    }

    #[test]
    fn filter_drops_off_angle_and_keeps_on_subject() {
        let subject = "WebAssembly Component Model WASI";
        let pack = vec![
            "https://dualmedia.com/fr/fonctionnalites-dinterop-2026".into(),
            "https://www.programme-television.org/news/series/ici-tout-commence-spoilers".into(),
            "https://m.economictimes.com/industry/cons-products/fmcg/delhi-hc-asks-fssai-to-hold-off-on-itc-licence-action-over-100-atta/articleshow/133498532.cms".into(),
            "https://blog.rust-lang.org/2026/01/01/wasm-component-model/".into(),
        ];
        let out = filter_pack_urls_for_subject(&pack, subject);
        assert!(
            out.iter()
                .any(|u| u.contains("rust-lang") && u.contains("wasm")),
            "on-subject url kept: {out:?}"
        );
        assert!(
            out.iter().any(|u| u.contains("dualmedia")),
            "non-off-angle publisher kept: {out:?}"
        );
        assert!(
            out.iter().all(|u| {
                !u.contains("programme-television")
                    && !u.contains("economictimes")
                    && !u.contains("ici-tout-commence")
            }),
            "off-angle dropped: {out:?}"
        );
    }

    #[test]
    fn body_abandons_soap_and_keeps_on_subject() {
        let subject = "Component Model WASI stability";
        assert!(body_abandons_subject(
            "Ici tout commence, Gary en garde a vue. #Rust",
            subject
        ));
        assert!(!body_abandons_subject(
            "Watching the Component Model and WASI land for builders.",
            subject
        ));
        assert!(!body_abandons_subject(
            "Builders care about modular secure systems and independent deploy.",
            subject
        ));
    }

    #[test]
    fn published_tweet_does_not_block_bare_propose_draft() {
        let dir = tempdir().expect("temp");
        let path = dir.path().join("runtime.db");
        let db = SourceDb::open(&path).expect("open");
        seed_chunk(
            &db,
            "ScyllaDB Rustlang driver for ScyllaDB Alternator throughput",
            "ScyllaDB ships a Rustlang driver for Alternator with higher throughput than the AWS SDK baseline.",
        );
        drop(db);

        let store = DraftStore::open(&path).expect("drafts");
        let mut shipped = stored_from_payload(DraftPayload {
            draft_id: "TWEET-20260828-000093".into(),
            subject: "NAPI-RS now supports the same Rust API for native Node and WebAssembly WASI"
                .into(),
            body: "Rust and WASM on Node via NAPI-RS for builders shipping native addons.".into(),
            model: "mock".into(),
            tokens_in: 1,
            tokens_out: 1,
            sources: Vec::new(),
            link_options: Vec::new(),
            research_pack: String::new(),
        });
        shipped.status = status::PUBLISHED.into();
        store.upsert(&shipped).expect("upsert tweet");
        drop(store);

        let (subject, _) =
            resolve_corpus_propose_brief(&path, ProposeSurface::Draft).expect("resolve draft");
        assert!(
            subject.to_ascii_lowercase().contains("scylla"),
            "published tweet must not exhaust bare /propose_draft, got {subject}"
        );
    }

    #[test]
    fn resolve_skips_topic_overlap_when_subject_differs() {
        let dir = tempdir().expect("temp");
        let path = dir.path().join("runtime.db");
        let db = SourceDb::open(&path).expect("open");
        seed_chunk(
            &db,
            "intent ship jpeg mozilla hacks web",
            "Mozilla Intent to Ship JPEG XL after a Rust decoder rewrite. Builders watch browser support.",
        );
        seed_chunk(
            &db,
            "agentic coding practical guide sourcegraph",
            "Sourcegraph published a practical guide to agentic coding in 2026 for teams shipping with AI.",
        );
        drop(db);

        let store = DraftStore::open(&path).expect("drafts");
        let mut shipped = stored_from_payload(DraftPayload {
            draft_id: "DRAFT-20260826-000114".into(),
            subject: "Mozilla wouldn't ship JPEG XL until someone rewrote the decoder in Rust."
                .into(),
            body:
                "JPEG XL decoder rewrite in Rust unlocks Mozilla shipping the format to browsers."
                    .into(),
            model: "mock".into(),
            tokens_in: 1,
            tokens_out: 1,
            sources: Vec::new(),
            link_options: Vec::new(),
            research_pack: String::new(),
        });
        shipped.status = status::PUBLISHED.into();
        store.upsert(&shipped).expect("upsert");
        drop(store);

        let (subject, _) =
            resolve_corpus_propose_brief(&path, ProposeSurface::Draft).expect("resolve");
        assert!(
            subject.to_ascii_lowercase().contains("agentic")
                || subject.to_ascii_lowercase().contains("sourcegraph"),
            "must skip JPEG overlap, got {subject}"
        );
    }

    #[test]
    fn body_has_slogan_mush_catches_not_about_and_not_just() {
        assert!(body_has_slogan_mush(
            "I'm watching how the web debates image formats, and it's not about which is better."
        ));
        assert!(body_has_slogan_mush(
            "This isn't just a shift in how open-source projects evolve."
        ));
        assert!(body_has_slogan_mush(
            "22.1k on GitHub? That's not just code, it's a new way to build."
        ));
        assert!(body_has_slogan_mush(
            "I'm watching how this change becomes habit."
        ));
        assert!(body_has_slogan_mush(
            "The framework's design is clean, its diffs honest, and its process clear. \
For a maintainer, this means less friction and more room to focus on what matters: \
writing code that feels right. And for those who care about systems, the 🦀 energy is unmistakable."
        ));
        assert!(body_has_slogan_mush(
            "I'm curious how this will shape the future of Rust tooling. Built for the long haul."
        ));
        assert!(!body_has_slogan_mush(
            "Mozilla shipped JPEG XL after a Rust decoder rewrite landed in Firefox."
        ));
    }

    #[test]
    fn strip_slogan_mush_drops_watching_habit_sentence_keeps_rest() {
        let body = "The Rust rewrite is a clear step forward.\n\n\
I'm watching how this change becomes habit. Builders who work with firmware will notice the difference. The Rust rewrite brings a new kind of care to the task.\n\n\
The core of this shift is in the analysis engine.";
        let out = strip_slogan_mush_sentences(body);
        assert!(
            !out.contains("becomes habit"),
            "mush sentence must drop: {out}"
        );
        assert!(
            out.contains("Builders who work with firmware"),
            "keep following sentences: {out}"
        );
        assert!(
            out.contains("The core of this shift"),
            "keep third para: {out}"
        );
    }

    #[test]
    fn strip_slogan_mush_preserves_paragraph_breaks() {
        let body = "Para one names Sätteri and the Astro ship.\n\n\
Para two says it's not about mindshare; it names pulldown-cmark instead.\n\n\
Para three closes with builders who ship.";
        let out = strip_slogan_mush_sentences(body);
        assert!(
            out.contains("Para one names Sätteri"),
            "keep clean paragraph: {out}"
        );
        assert!(
            !out.contains("not about mindshare"),
            "drop mush paragraph: {out}"
        );
        assert!(
            out.contains("Para one names Sätteri and the Astro ship")
                && out.contains("Para three closes with builders who ship")
                && out.contains("\n\n"),
            "preserve \\n\\n aeration: {out:?}"
        );
    }

    #[test]
    fn strip_slogan_mush_does_not_split_on_newlines_inside_paragraph() {
        let body = "Line one still here.\nLine two still here.";
        let out = strip_slogan_mush_sentences(body);
        assert_eq!(out, "Line one still here. Line two still here");
    }
}

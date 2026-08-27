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

const CANDIDATE_LIMIT: u32 = 80;
const USED_SUBJECT_LIMIT: usize = 200;
const MIN_SUBJECT_CHARS: usize = 8;
const MAX_SUBJECT_CHARS: usize = 160;
const MAX_GROUNDING_CHARS: usize = 900;
const MIN_CHUNK_TEXT: usize = 40;

/// Resolve a concrete subject + instructions from corpus (no catalog fallback).
///
/// Skips corpus angles whose subject already appears on an in-flight or shipped
/// `DRAFT-` / `TWEET-` row so bare propose does not re-open the same story.
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
    let used = load_used_propose_subjects(db_path);
    let Some(picked) = pick_corpus_angle(&chunks, &used) else {
        if used.is_empty() {
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

/// Tighter brief for one post-write retry after off-subject drift.
#[must_use]
pub fn tighter_corpus_propose_instructions(subject: &str, surface: ProposeSurface) -> String {
    let base = match surface {
        ProposeSurface::Draft => {
            "Rewrite one company-page LinkedIn post strictly on the subject below. \
Ignore entertainment spoilers and unrelated companies. Stay on this subject only."
        }
        ProposeSurface::Tweet => {
            "Rewrite one company X tweet strictly on the subject below. \
Ignore entertainment spoilers and unrelated companies. Stay on this subject only."
        }
    };
    format!("{base}\n\nSubject lock: {subject}")
}

struct PickedAngle {
    subject: String,
    grounding: String,
}

fn pick_corpus_angle(chunks: &[ChunkRecord], used: &HashSet<String>) -> Option<PickedAngle> {
    let mut seen: Vec<String> = Vec::new();
    for c in chunks {
        let subject = angle_subject(c)?;
        let key = subject.to_ascii_lowercase();
        if used.contains(&key) {
            continue;
        }
        if seen.iter().any(|s| s == &key) {
            continue;
        }
        if !chunk_text_usable(&c.text) {
            continue;
        }
        seen.push(key);
        let grounding = clip_chars(c.text.trim(), MAX_GROUNDING_CHARS);
        return Some(PickedAngle { subject, grounding });
    }
    None
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
    fn tighter_instructions_lock_subject() {
        let s = tighter_corpus_propose_instructions("Rust async", ProposeSurface::Tweet);
        assert!(s.contains("Subject lock: Rust async"));
        assert!(s.contains("Stay on this subject") || s.contains("strictly on the subject"));
    }
}

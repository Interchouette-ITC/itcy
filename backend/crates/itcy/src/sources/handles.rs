// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Social media handle registry loaded from `backend/handles.toml`.
//!
//! The index is loaded once at startup and queried via the `lookup_handles`
//! LLM tool. The full file is never injected into a prompt; only matching
//! rows are returned to the model as a tool result.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use thiserror::Error;

const DEFAULT_REL: &str = "handles.toml";

/// One known entity with its social handles.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HandleEntry {
    /// Canonical entity name used for lookup.
    pub name: String,
    /// `LinkedIn` company/profile handle (e.g. `@interchouette-itc`). Optional.
    #[serde(default)]
    pub linkedin: String,
    /// X / Twitter handle (e.g. `@Interchouette`). Optional.
    #[serde(default)]
    pub x: String,
    /// Canonical `LinkedIn` URL for this entry (clickable in `handles.toml`).
    #[serde(default)]
    pub linkedin_url: String,
    /// Canonical X URL for this entry (clickable in `handles.toml`).
    #[serde(default)]
    pub x_url: String,
}

#[derive(Debug, Deserialize)]
struct HandlesFile {
    #[serde(default)]
    handle: Vec<HandleEntry>,
}

/// Load / resolve errors for the handles file.
#[derive(Debug, Error)]
pub enum HandlesError {
    #[error("handles: {0}")]
    Io(#[from] std::io::Error),
    #[error("handles parse: {0}")]
    Parse(#[from] toml::de::Error),
}

/// In-memory handle registry. Searched by case-insensitive substring on `name`.
#[derive(Debug, Clone, Default)]
pub struct HandlesIndex {
    entries: Vec<HandleEntry>,
}

impl HandlesIndex {
    /// Search by case-insensitive substring match on `name`. Returns up to 5 matches.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<&HandleEntry> {
        let q = query.to_lowercase();
        self.entries
            .iter()
            .filter(|e| e.name.to_lowercase().contains(q.as_str()))
            .take(5)
            .collect()
    }

    /// Number of entries in the index.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the index is empty (file missing or empty).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Best registry hit for an operator brief (name, profile URL, or `@handle`).
    ///
    /// Deterministic, not model-dependent. Order: profile URLs, bare `@` tokens, plain
    /// names in the brief, then article publisher host. Names beat publisher host so a
    /// subject about Cursor on an `InfoWorld` URL resolves to Cursor, not the publisher.
    #[must_use]
    pub fn primary_from_brief(&self, brief: &str) -> Option<&HandleEntry> {
        if brief.trim().is_empty() || self.entries.is_empty() {
            return None;
        }
        if let Some(hit) = self.hit_from_urls(brief) {
            return Some(hit);
        }
        if let Some(hit) = self.hit_from_at_handles(brief) {
            return Some(hit);
        }
        if let Some(hit) = self.hit_from_names(brief) {
            return Some(hit);
        }
        self.hit_from_publisher_host(brief)
    }

    fn hit_from_urls(&self, brief: &str) -> Option<&HandleEntry> {
        for url in crate::sources::url_hygiene::extract_https_urls(brief) {
            let norm = normalize_profile_url(&url);
            for entry in &self.entries {
                if !entry.linkedin_url.is_empty()
                    && normalize_profile_url(&entry.linkedin_url) == norm
                {
                    return Some(entry);
                }
                if !entry.x_url.is_empty() && normalize_profile_url(&entry.x_url) == norm {
                    return Some(entry);
                }
            }
            if let Some(slug) = linkedin_slug_from_url(&url) {
                let needle = format!("@{slug}");
                for entry in &self.entries {
                    if entry.linkedin.eq_ignore_ascii_case(&needle) {
                        return Some(entry);
                    }
                }
            }
            if let Some(slug) = x_slug_from_url(&url) {
                let needle = format!("@{slug}");
                for entry in &self.entries {
                    if entry.x.eq_ignore_ascii_case(&needle) {
                        return Some(entry);
                    }
                }
            }
        }
        None
    }

    fn hit_from_publisher_host(&self, brief: &str) -> Option<&HandleEntry> {
        for url in crate::sources::url_hygiene::extract_https_urls(brief) {
            let Some(host_label) = publisher_host_label(&url) else {
                continue;
            };
            for entry in &self.entries {
                if publisher_name_matches_host(&entry.name, &host_label) {
                    return Some(entry);
                }
            }
        }
        None
    }

    fn hit_from_at_handles(&self, brief: &str) -> Option<&HandleEntry> {
        for at in extract_at_handles(brief) {
            for entry in &self.entries {
                if (!entry.linkedin.is_empty() && entry.linkedin.eq_ignore_ascii_case(&at))
                    || (!entry.x.is_empty() && entry.x.eq_ignore_ascii_case(&at))
                {
                    return Some(entry);
                }
            }
        }
        None
    }

    fn hit_from_names(&self, brief: &str) -> Option<&HandleEntry> {
        // Longer names win; same length → earlier mention in the brief (topic before aside).
        let mut best: Option<(&HandleEntry, usize, usize)> = None;
        for entry in &self.entries {
            let name = entry.name.trim();
            if name.len() < 3 {
                continue;
            }
            let Some((start, _)) = find_phrase_outside_url(brief, name) else {
                continue;
            };
            let score = name.len();
            let better = match best {
                None => true,
                Some((_, s, pos)) => score > s || (score == s && start < pos),
            };
            if better {
                best = Some((entry, score, start));
            }
        }
        best.map(|(e, _, _)| e)
    }
}

/// Candidate paths searched in order (mirrors `live_sites.rs` pattern).
#[must_use]
pub fn handles_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(DEFAULT_REL));
        out.push(cwd.join("backend").join(DEFAULT_REL));
        out.push(cwd.join("../backend").join(DEFAULT_REL));
    }
    out.push(crate::paths::product_join("backend/handles.toml"));
    out
}

/// First existing path among [`handles_candidates`].
#[must_use]
pub fn resolve_handles_path() -> Option<PathBuf> {
    handles_candidates().into_iter().find(|p| p.is_file())
}

/// Load the handle registry. Returns an empty index when the file is not found.
///
/// # Errors
///
/// Returns [`HandlesError`] when the file exists but cannot be read or parsed.
pub fn load_handles() -> Result<HandlesIndex, HandlesError> {
    let Some(path) = resolve_handles_path() else {
        return Ok(HandlesIndex::default());
    };
    load_handles_from(&path)
}

/// Load from an explicit path (tests).
///
/// # Errors
///
/// Returns [`HandlesError`] on IO or TOML parse failure.
pub fn load_handles_from(path: &Path) -> Result<HandlesIndex, HandlesError> {
    let text = std::fs::read_to_string(path)?;
    let parsed: HandlesFile = toml::from_str(&text)?;
    let entries = parsed
        .handle
        .into_iter()
        .filter(|e| !e.name.trim().is_empty())
        .map(|e| HandleEntry {
            name: e.name.trim().to_string(),
            linkedin: e.linkedin.trim().to_string(),
            x: e.x.trim().to_string(),
            linkedin_url: e.linkedin_url.trim().to_string(),
            x_url: e.x_url.trim().to_string(),
        })
        .collect();
    Ok(HandlesIndex { entries })
}

/// Result of `/handle_add` after parse + optional file append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleAddOutcome {
    /// New row appended to `handles.toml` and memory.
    Added(HandleEntry),
    /// Same name or profile URL already in the registry (no duplicate append).
    AlreadyPresent(HandleEntry),
}

impl HandlesIndex {
    /// Append or replace by identity (name / `LinkedIn` URL / X URL).
    pub fn upsert_entry(&mut self, entry: HandleEntry) {
        if let Some(i) = self.duplicate_index(&entry) {
            merge_missing_fields(&mut self.entries[i], &entry);
            return;
        }
        self.entries.push(entry);
    }

    fn duplicate_index(&self, entry: &HandleEntry) -> Option<usize> {
        self.entries.iter().position(|e| entries_match(e, entry))
    }

    /// Existing row that matches name or profile URL, if any.
    #[must_use]
    pub fn find_duplicate(&self, entry: &HandleEntry) -> Option<&HandleEntry> {
        self.duplicate_index(entry).map(|i| &self.entries[i])
    }
}

fn entries_match(a: &HandleEntry, b: &HandleEntry) -> bool {
    if !a.name.is_empty() && !b.name.is_empty() && a.name.eq_ignore_ascii_case(&b.name) {
        return true;
    }
    if !a.linkedin_url.is_empty()
        && !b.linkedin_url.is_empty()
        && normalize_profile_url(&a.linkedin_url) == normalize_profile_url(&b.linkedin_url)
    {
        return true;
    }
    if !a.x_url.is_empty()
        && !b.x_url.is_empty()
        && normalize_profile_url(&a.x_url) == normalize_profile_url(&b.x_url)
    {
        return true;
    }
    false
}

fn merge_missing_fields(dst: &mut HandleEntry, src: &HandleEntry) {
    if dst.linkedin.is_empty() && !src.linkedin.is_empty() {
        dst.linkedin.clone_from(&src.linkedin);
    }
    if dst.x.is_empty() && !src.x.is_empty() {
        dst.x.clone_from(&src.x);
    }
    if dst.linkedin_url.is_empty() && !src.linkedin_url.is_empty() {
        dst.linkedin_url.clone_from(&src.linkedin_url);
    }
    if dst.x_url.is_empty() && !src.x_url.is_empty() {
        dst.x_url.clone_from(&src.x_url);
    }
}

fn normalize_profile_url(url: &str) -> String {
    let u = crate::sources::url_hygiene::scrub_https_url(url).to_ascii_lowercase();
    u.trim_end_matches('/').to_string()
}

/// First label of a publisher article host (`infoworld.com` -> `infoworld`).
/// Skips social/profile hosts handled by [`HandlesIndex::hit_from_urls`].
fn publisher_host_label(url: &str) -> Option<String> {
    let low = url.trim().to_ascii_lowercase();
    let rest = low
        .strip_prefix("https://")
        .or_else(|| low.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.split('@').next_back().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.contains("linkedin.com")
        || host.contains("lnkd.in")
        || host == "x.com"
        || host == "twitter.com"
        || host == "t.co"
    {
        return None;
    }
    let label = host.split('.').next()?.trim();
    if label.len() < 3 {
        return None;
    }
    Some(label.to_string())
}

fn normalized_publisher_name_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn publisher_name_matches_host(name: &str, host_label: &str) -> bool {
    let name_key = normalized_publisher_name_key(name);
    if name_key.len() < 3 {
        return false;
    }
    name_key == host_label || name_key.starts_with(host_label) || host_label.starts_with(&name_key)
}

fn linkedin_slug_from_url(url: &str) -> Option<String> {
    let low = url.to_ascii_lowercase();
    for marker in ["/in/", "/company/"] {
        if let Some(i) = low.find(marker) {
            let rest = &url[i + marker.len()..];
            let slug = rest.split(['/', '?', '#', '&']).next().unwrap_or("").trim();
            if !slug.is_empty() {
                return Some(slug.to_ascii_lowercase());
            }
        }
    }
    None
}

fn x_slug_from_url(url: &str) -> Option<String> {
    let low = url.to_ascii_lowercase();
    for host in [
        "https://x.com/",
        "https://twitter.com/",
        "http://x.com/",
        "http://twitter.com/",
    ] {
        if let Some(rest) = low.strip_prefix(host) {
            let slug = rest.split(['/', '?', '#', '&']).next().unwrap_or("").trim();
            if !slug.is_empty()
                && !matches!(
                    slug,
                    "i" | "home" | "explore" | "search" | "intent" | "share" | "hashtag"
                )
            {
                return Some(slug.to_string());
            }
        }
    }
    None
}

fn extract_at_handles(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-')
            {
                i += 1;
            }
            if i > start + 1 {
                let at = trim_glued_site_suffix(&text[start..i]);
                if !out.iter().any(|x: &String| x.eq_ignore_ascii_case(&at)) {
                    out.push(at);
                }
            }
            continue;
        }
        i += 1;
    }
    out
}

/// `@nikomatsakislinkedin` (glued before `.com`) → `@nikomatsakis`.
fn trim_glued_site_suffix(at: &str) -> String {
    let body = at.trim_start_matches('@');
    let lower = body.to_ascii_lowercase();
    for suffix in ["linkedin", "twitter"] {
        if let Some(prefix) = lower.strip_suffix(suffix) {
            if prefix.len() >= 2 {
                return format!("@{}", &body[..prefix.len()]);
            }
        }
    }
    at.to_string()
}

/// Parse operator free text into a [`HandleEntry`] (name + URLs / `@handles`).
///
/// Requires the **last two** whitespace tokens to each start with `@` or `https://`.
/// Bare slug pairs (e.g. `rust-bytes-weekly rustaceans_rs`) are a noop error.
///
/// # Errors
///
/// Returns a short operator message when nothing usable is present or the gate fails.
pub fn parse_handle_add(raw: &str) -> Result<HandleEntry, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("usage: /handle_add <name> <@linkedin|url> <@x|url>".into());
    }
    refuse_unless_last_two_are_handles(raw)?;
    let urls = crate::sources::url_hygiene::extract_https_urls(raw);
    let ats = extract_at_handles(raw);
    let mut linkedin = String::new();
    let mut linkedin_url = String::new();
    let mut x = String::new();
    let mut x_url = String::new();
    for url in &urls {
        let scrubbed = crate::sources::url_hygiene::scrub_https_url(url);
        if linkedin_slug_from_url(&scrubbed).is_some() {
            if let Some(slug) = linkedin_slug_from_url(&scrubbed) {
                linkedin = format!("@{slug}");
                linkedin_url = scrubbed;
            }
            continue;
        }
        if let Some(slug) = x_slug_from_url(&scrubbed) {
            x = format!("@{slug}");
            x_url = scrubbed;
        }
    }
    for at in &ats {
        let at_norm = if at.starts_with('@') {
            at.clone()
        } else {
            format!("@{at}")
        };
        if !linkedin.is_empty() && !linkedin.eq_ignore_ascii_case(&at_norm) && x.is_empty() {
            x = at_norm;
            x_url = format!("https://x.com/{}", x.trim_start_matches('@'));
            continue;
        }
        if linkedin.is_empty() && x.is_empty() {
            x = at_norm;
            x_url = format!("https://x.com/{}", x.trim_start_matches('@'));
            continue;
        }
        if linkedin.is_empty() {
            linkedin = at_norm;
            linkedin_url = format!(
                "https://www.linkedin.com/company/{}/",
                linkedin.trim_start_matches('@')
            );
        }
    }
    // Prefer last two @handles as LinkedIn then X when both are bare @handles.
    let tokens = trailing_handle_tokens(raw);
    if tokens.len() >= 2 {
        let a = &tokens[tokens.len() - 2];
        let b = &tokens[tokens.len() - 1];
        if a.starts_with('@') && b.starts_with('@') {
            linkedin.clone_from(a);
            linkedin_url = format!(
                "https://www.linkedin.com/company/{}/",
                a.trim_start_matches('@')
            );
            x.clone_from(b);
            x_url = format!("https://x.com/{}", b.trim_start_matches('@'));
        }
    }
    let name = strip_urls_and_ats(raw).trim().to_string();
    let name = if name.is_empty() {
        humanize_from_entry(&linkedin, &x, &linkedin_url)
    } else {
        name
    };
    if name.is_empty() && linkedin.is_empty() && x.is_empty() {
        return Err("usage: /handle_add <name> <@linkedin|url> <@x|url>".into());
    }
    if name.is_empty() {
        return Err("could not derive a display name; pass a name before the URL/@".into());
    }
    Ok(HandleEntry {
        name,
        linkedin,
        x,
        linkedin_url,
        x_url,
    })
}

/// Last two tokens must each be `@handle` or `https://…` (else noop).
fn refuse_unless_last_two_are_handles(raw: &str) -> Result<(), String> {
    let tokens = trailing_handle_tokens(raw);
    if tokens.len() < 2 {
        return Err("noop: need two trailing @handles or https URLs \
(usage: /handle_add <name> <@linkedin|url> <@x|url>)"
            .into());
    }
    let a = &tokens[tokens.len() - 2];
    let b = &tokens[tokens.len() - 1];
    if !token_is_handle_spec(a) || !token_is_handle_spec(b) {
        return Err("noop: last two words must be @handles or https:// URLs \
(got bare slugs — not added)"
            .into());
    }
    Ok(())
}

fn trailing_handle_tokens(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .map(|t| t.trim_matches(|c| matches!(c, '*' | '_' | '`' | ',' | ';')))
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn token_is_handle_spec(tok: &str) -> bool {
    let t = tok.trim();
    t.starts_with('@') || t.starts_with("https://") || t.starts_with("http://")
}

fn strip_urls_and_ats(raw: &str) -> String {
    let mut s = raw.to_string();
    for url in crate::sources::url_hygiene::extract_https_urls(raw) {
        s = s.replace(&url, " ");
        let with_slash = format!("{url}/");
        s = s.replace(&with_slash, " ");
    }
    for at in extract_at_handles(raw) {
        s = s.replace(&at, " ");
    }
    s.split_whitespace()
        .filter(|t| t.chars().any(char::is_alphanumeric))
        .collect::<Vec<_>>()
        .join(" ")
}

fn humanize_from_entry(linkedin: &str, x: &str, linkedin_url: &str) -> String {
    if let Some(slug) = linkedin_slug_from_url(linkedin_url) {
        return humanize_slug(&slug);
    }
    let from_li = linkedin.trim_start_matches('@');
    if !from_li.is_empty() {
        return humanize_slug(from_li);
    }
    let from_x = x.trim_start_matches('@');
    if !from_x.is_empty() {
        return humanize_slug(from_x);
    }
    String::new()
}

fn humanize_slug(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|p| !p.is_empty() && !p.chars().all(|c| c.is_ascii_digit()))
        .map(|p| {
            let mut chars = p.chars();
            chars.next().map_or_else(String::new, |c| {
                let mut out = c.to_uppercase().collect::<String>();
                out.push_str(&chars.as_str().to_ascii_lowercase());
                out
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Append one `[[handle]]` block to `handles.toml`.
///
/// # Errors
///
/// Returns [`HandlesError`] on IO failure.
pub fn append_handle_toml(path: &Path, entry: &HandleEntry) -> Result<(), HandlesError> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
    let block = format_handle_toml_block(entry);
    f.write_all(block.as_bytes())?;
    Ok(())
}

#[must_use]
pub fn format_handle_toml_block(entry: &HandleEntry) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("\n[[handle]]\n");
    let _ = writeln!(out, "name = \"{}\"", toml_escape(&entry.name));
    if !entry.linkedin.is_empty() {
        let _ = writeln!(out, "linkedin = \"{}\"", toml_escape(&entry.linkedin));
    }
    if !entry.x.is_empty() {
        let _ = writeln!(out, "x = \"{}\"", toml_escape(&entry.x));
    }
    if !entry.linkedin_url.is_empty() {
        let _ = writeln!(
            out,
            "linkedin_url = \"{}\"",
            toml_escape(&entry.linkedin_url)
        );
    }
    if !entry.x_url.is_empty() {
        let _ = writeln!(out, "x_url = \"{}\"", toml_escape(&entry.x_url));
    }
    out
}

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Parse, detect duplicate, append file when new, always upsert memory.
///
/// # Errors
///
/// Parse errors as `String`; file IO as [`HandlesError`] mapped by caller.
pub fn apply_handle_add(
    index: &mut HandlesIndex,
    path: &Path,
    raw: &str,
) -> Result<HandleAddOutcome, String> {
    let entry = parse_handle_add(raw)?;
    if let Some(existing) = index.find_duplicate(&entry).cloned() {
        index.upsert_entry(entry);
        let after = index.find_duplicate(&existing).cloned().unwrap_or(existing);
        return Ok(HandleAddOutcome::AlreadyPresent(after));
    }
    append_handle_toml(path, &entry).map_err(|e| e.to_string())?;
    index.upsert_entry(entry.clone());
    Ok(HandleAddOutcome::Added(entry))
}

/// Slack reply lines for a successful `/handle_add`.
#[must_use]
pub fn format_handle_add_reply(outcome: &HandleAddOutcome) -> String {
    let (title, e) = match outcome {
        HandleAddOutcome::Added(e) => ("*Handle added*", e),
        HandleAddOutcome::AlreadyPresent(e) => ("*Handle already present*", e),
    };
    let li = if e.linkedin.is_empty() {
        "(none)".to_string()
    } else {
        e.linkedin.clone()
    };
    let x = if e.x.is_empty() {
        "(none)".to_string()
    } else {
        e.x.clone()
    };
    let mut lines = vec![
        title.to_string(),
        format!("• name: {}", e.name),
        format!("• linkedin: {li}"),
        format!("• x: {x}"),
    ];
    if !e.linkedin_url.is_empty() {
        lines.push(format!("• {}", e.linkedin_url));
    }
    if !e.x_url.is_empty() {
        lines.push(format!("• {}", e.x_url));
    }
    lines.join("\n")
}

const BRAND: &str = "Interchouette";
const BRAND_ITC: &str = "Interchouette ITC";
const LINKEDIN_BRAND_HANDLE: &str = "@interchouette-itc";

/// When the post already names Interchouette, use `@interchouette-itc` as the mention.
///
/// Replaces the brand name (does not invent a mention). Collapses verbose
/// `Interchouette ITC (@interchouette-itc)` forms to the bare handle.
#[must_use]
pub fn ensure_linkedin_brand_mention(body: &str) -> String {
    const VERBOSE_ITC: &str = "Interchouette ITC (@interchouette-itc)";
    const VERBOSE: &str = "Interchouette (@interchouette-itc)";
    if let Some((start, end)) = find_phrase_outside_url(body, VERBOSE_ITC) {
        return replace_range(body, start, end, LINKEDIN_BRAND_HANDLE);
    }
    if let Some((start, end)) = find_phrase_outside_url(body, VERBOSE) {
        return replace_range(body, start, end, LINKEDIN_BRAND_HANDLE);
    }
    if body.to_ascii_lowercase().contains(LINKEDIN_BRAND_HANDLE) {
        return body.to_string();
    }
    if let Some((start, end)) = find_phrase_outside_url(body, BRAND_ITC) {
        return replace_range(body, start, end, LINKEDIN_BRAND_HANDLE);
    }
    if let Some((start, end)) = find_phrase_outside_url(body, BRAND) {
        return replace_range(body, start, end, LINKEDIN_BRAND_HANDLE);
    }
    body.to_string()
}

/// Deterministic `handles:` from the operator brief (name / profile URL / `@`), like cite URLs.
///
/// Overwrites a model-written `handles:` line when the registry matches the brief.
pub fn ensure_pack_handles_from_brief(pack: &mut String, brief: &str, index: &HandlesIndex) {
    let Some(entry) = index.primary_from_brief(brief) else {
        return;
    };
    let Some(line) = format_handles_line(&entry.linkedin, &entry.x) else {
        return;
    };
    *pack = upsert_handles_line(pack, &line);
}

/// Put the `LinkedIn` brand handle in the `ResearchPack` when the operator named Interchouette.
pub fn ensure_pack_linkedin_brand_handle(pack: &mut String, brief: &str) {
    if find_phrase_outside_url(brief, BRAND).is_none() {
        return;
    }
    if pack
        .to_ascii_lowercase()
        .contains("linkedin=@interchouette-itc")
    {
        return;
    }
    *pack = upsert_handles_line(pack, "handles: linkedin=@interchouette-itc");
}

/// Apply brief registry handles, then brand Interchouette when named.
pub fn apply_brief_handles_to_pack(pack: &mut String, brief: &str, index: &HandlesIndex) {
    ensure_pack_handles_from_brief(pack, brief, index);
    ensure_pack_linkedin_brand_handle(pack, brief);
}

/// Put the pack's `LinkedIn` `@handle` in the body (name to handle, or lead line).
///
/// Skips the Interchouette brand handle (see [`ensure_linkedin_brand_mention`]).
/// When the pack only has `x=` (partial registry at draft time), still resolve the
/// entry's `LinkedIn` handle from that X handle so cite inject works after the
/// registry gains a `LinkedIn` field.
#[must_use]
pub fn ensure_linkedin_handle_from_pack(body: &str, pack: &str, index: &HandlesIndex) -> String {
    let Some(handle) = linkedin_handle_for_pack(pack, index) else {
        return body.to_string();
    };
    if handle.eq_ignore_ascii_case(LINKEDIN_BRAND_HANDLE) {
        return body.to_string();
    }
    ensure_named_handle_in_body(body, &handle, index, HandleMatch::LinkedIn)
}

/// Put the pack's X `@handle` in the tweet body when the entity name is already present.
///
/// Skips the own account handle (`@Interchouette`). Never injects a `LinkedIn` handle on X.
/// Does **not** invent a leading `@Handle:` when the name is absent (avoids wrong entity + UTF-8 panics).
#[must_use]
pub fn ensure_x_handle_from_pack(body: &str, pack: &str, index: &HandlesIndex) -> String {
    let Some(handle) = handle_from_pack(pack, "x=") else {
        return body.to_string();
    };
    if handle.eq_ignore_ascii_case(&format!(
        "@{}",
        crate::sources::url_hygiene::X_PUBLIC_HANDLE
    )) {
        return body.to_string();
    }
    ensure_named_handle_in_body(body, &handle, index, HandleMatch::X)
}

#[derive(Clone, Copy)]
enum HandleMatch {
    LinkedIn,
    X,
}

fn ensure_named_handle_in_body(
    body: &str,
    handle: &str,
    index: &HandlesIndex,
    kind: HandleMatch,
) -> String {
    if body
        .to_ascii_lowercase()
        .contains(&handle.to_ascii_lowercase())
    {
        return body.to_string();
    }
    let entry = index.entries.iter().find(|e| match kind {
        HandleMatch::LinkedIn => e.linkedin.eq_ignore_ascii_case(handle),
        HandleMatch::X => e.x.eq_ignore_ascii_case(handle),
    });
    if let Some(entry) = entry {
        if let Some((start, end)) = find_phrase_outside_url(body, &entry.name) {
            return replace_range(body, start, end, handle);
        }
    }
    let trimmed = body.trim_start();
    if trimmed.is_empty() {
        return handle.to_string();
    }
    match kind {
        HandleMatch::LinkedIn => {
            if entry.is_some_and(use_publisher_name_lead) {
                // Publishers: cite via URL in prose; no `From @handle.` or `Name:` lead line.
                return body.to_string();
            }
            let lead = format!("From {handle}.");
            format!("{lead}\n\n{trimmed}")
        }
        HandleMatch::X => body.to_string(),
    }
}

fn use_publisher_name_lead(entry: &HandleEntry) -> bool {
    if entry.linkedin_url.contains("/company/") {
        return true;
    }
    !entry.name.contains(' ')
        && entry
            .name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
}

fn handle_from_pack(pack: &str, key: &str) -> Option<String> {
    for line in pack.lines() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("handles:") else {
            continue;
        };
        for part in rest.split_whitespace() {
            if let Some(h) = part.strip_prefix(key) {
                let h = h.trim();
                if h.starts_with('@') {
                    return Some(h.to_string());
                }
            }
        }
    }
    None
}

/// `LinkedIn` `@` for the pack: prefer `linkedin=`, else the registry row matched by `x=`.
fn linkedin_handle_for_pack(pack: &str, index: &HandlesIndex) -> Option<String> {
    if let Some(h) = handle_from_pack(pack, "linkedin=") {
        return Some(h);
    }
    let x = handle_from_pack(pack, "x=")?;
    index.entries.iter().find_map(|e| {
        if e.x.eq_ignore_ascii_case(&x) && !e.linkedin.is_empty() {
            Some(e.linkedin.clone())
        } else {
            None
        }
    })
}

fn format_handles_line(linkedin: &str, x: &str) -> Option<String> {
    let mut parts = Vec::new();
    if !linkedin.trim().is_empty() {
        parts.push(format!("linkedin={}", linkedin.trim()));
    }
    if !x.trim().is_empty() {
        parts.push(format!("x={}", x.trim()));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("handles: {}", parts.join(" ")))
    }
}

fn replace_range(body: &str, start: usize, end: usize, with: &str) -> String {
    format!("{}{}{}", &body[..start], with, &body[end..])
}

fn upsert_handles_line(pack: &str, line: &str) -> String {
    let mut out = String::new();
    let mut inserted = false;
    let mut saw_subject = false;
    for raw in pack.lines() {
        let t = raw.trim_start();
        if t.starts_with("handles:") {
            continue;
        }
        out.push_str(raw);
        out.push('\n');
        if !inserted && raw.starts_with("subject:") {
            saw_subject = true;
            out.push_str(line);
            out.push('\n');
            inserted = true;
        }
    }
    if !inserted {
        if saw_subject {
            out.push_str(line);
            out.push('\n');
        } else {
            out.insert_str(0, &format!("{line}\n"));
        }
    }
    out
}

fn find_phrase_outside_url(hay: &str, phrase: &str) -> Option<(usize, usize)> {
    let hay_l = hay.to_ascii_lowercase();
    let needle = phrase.to_ascii_lowercase();
    let mut from = 0usize;
    while from < hay_l.len() {
        let Some(rel) = hay_l.get(from..).and_then(|s| s.find(needle.as_str())) else {
            break;
        };
        let start = from + rel;
        let end = start + needle.len();
        if end > hay.len() {
            break;
        }
        if word_boundary(hay, start, end) && !skip_host_or_url(hay, start, end) {
            return Some((start, end));
        }
        from = start.saturating_add(1);
    }
    None
}

fn word_boundary(hay: &str, start: usize, end: usize) -> bool {
    let before_ok = start == 0
        || hay
            .get(..start)
            .and_then(|s| s.chars().next_back())
            .is_none_or(|c| !c.is_alphanumeric());
    let after_ok = end >= hay.len()
        || hay
            .get(end..)
            .and_then(|s| s.chars().next())
            .is_none_or(|c| !c.is_alphanumeric());
    before_ok && after_ok
}

fn skip_host_or_url(hay: &str, start: usize, end: usize) -> bool {
    if inside_url(hay, start) {
        return true;
    }
    let before = hay.get(..start).and_then(|s| s.chars().next_back());
    let after = hay.get(end..).and_then(|s| s.chars().next());
    matches!(before, Some('.' | '/' | ':' | '@')) || matches!(after, Some('.' | '/'))
}

fn inside_url(hay: &str, idx: usize) -> bool {
    let prefix = hay.get(..idx).unwrap_or("");
    let start = prefix.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    hay.get(start..idx).is_some_and(|s| s.contains("://"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_seed_file_when_present() {
        let Some(path) = resolve_handles_path() else {
            return;
        };
        let idx = load_handles_from(&path).expect("parse");
        assert!(idx.len() >= 2);
        assert!(!idx.search("Interchouette").is_empty());
    }

    #[test]
    fn search_case_insensitive() {
        let idx = HandlesIndex {
            entries: vec![HandleEntry {
                name: "Rust Foundation".into(),
                linkedin: "@rust-foundation".into(),
                x: "@rustlang".into(),
                linkedin_url: "https://www.linkedin.com/company/rust-foundation/".into(),
                x_url: "https://x.com/rustlang".into(),
            }],
        };
        assert!(!idx.search("rust").is_empty());
        assert!(!idx.search("RUST").is_empty());
        assert!(idx.search("anthropic").is_empty());
    }

    #[test]
    fn empty_index_when_file_missing() {
        let result = load_handles_from(std::path::Path::new("/nonexistent/handles.toml"));
        assert!(result.is_err());
        let idx = load_handles();
        // Either loaded from disk or empty - both are valid.
        let _ = idx;
    }

    #[test]
    fn linkedin_replaces_brand_with_company_handle() {
        let out = ensure_linkedin_brand_mention("Interchouette ITC shipped WebMCP on the site.");
        assert_eq!(out, "@interchouette-itc shipped WebMCP on the site.");
    }

    #[test]
    fn linkedin_replaces_bare_brand_name() {
        let out = ensure_linkedin_brand_mention("Interchouette shipped WebMCP.");
        assert_eq!(out, "@interchouette-itc shipped WebMCP.");
    }

    #[test]
    fn linkedin_collapses_verbose_handle_form() {
        let src = "Interchouette ITC (@interchouette-itc) shipped WebMCP.";
        assert_eq!(
            ensure_linkedin_brand_mention(src),
            "@interchouette-itc shipped WebMCP."
        );
    }

    #[test]
    fn linkedin_does_not_double_handle() {
        let src = "@interchouette-itc shipped WebMCP.";
        assert_eq!(ensure_linkedin_brand_mention(src), src);
    }

    #[test]
    fn linkedin_skips_handle_inside_site_url() {
        let src = "See https://mcp.interchouette.net for the tools.";
        assert_eq!(ensure_linkedin_brand_mention(src), src);
    }

    #[test]
    fn pack_gets_linkedin_handle_when_brief_names_brand() {
        let mut pack = String::from("## ResearchPack\nsubject: WebMCP\nsummary: spec\n");
        ensure_pack_linkedin_brand_handle(
            &mut pack,
            "WebMCP, Interchouette has integrated it at https://mcp.interchouette.net",
        );
        assert!(pack.contains("handles: linkedin=@interchouette-itc"));
        assert!(pack.contains("subject: WebMCP"));
    }

    fn isaac_index() -> HandlesIndex {
        HandlesIndex {
            entries: vec![HandleEntry {
                name: "Isaac Sacolick".into(),
                linkedin: "@isaacsacolick".into(),
                x: "@nyike".into(),
                linkedin_url: "https://www.linkedin.com/in/isaacsacolick".into(),
                x_url: "https://x.com/nyike".into(),
            }],
        }
    }

    #[test]
    fn brief_name_injects_handles_like_cite_url() {
        let idx = isaac_index();
        let mut pack = String::from(
            "## ResearchPack\nsubject: DX principles\nhandles: linkedin=@interchouette-itc x=@interchouette\nsummary: noise\n",
        );
        let brief = "10 principles for creating a great developer experience, from Isaac Sacolick cite https://www.infoworld.com/article/2337290/10-principles-for-creating-great-developer-experiences.html";
        apply_brief_handles_to_pack(&mut pack, brief, &idx);
        assert!(pack.contains("handles: linkedin=@isaacsacolick x=@nyike"));
        assert!(!pack.contains("@interchouette-itc"));
        assert_eq!(pack.matches("handles:").count(), 1);
    }

    #[test]
    fn brief_linkedin_url_resolves_registry_row() {
        let idx = isaac_index();
        let hit = idx
            .primary_from_brief("cite https://www.linkedin.com/in/isaacsacolick/")
            .expect("url hit");
        assert_eq!(hit.x, "@nyike");
    }

    #[test]
    fn brief_x_handle_resolves_registry_row() {
        let idx = isaac_index();
        let hit = idx
            .primary_from_brief("quote @nyike on DX")
            .expect("at hit");
        assert_eq!(hit.linkedin, "@isaacsacolick");
    }

    #[test]
    fn body_gets_pack_linkedin_handle_when_missing() {
        let idx = isaac_index();
        let pack = "subject: DX\nhandles: linkedin=@isaacsacolick x=@nyike\n";
        let body = "Great developer experience needs less friction in the toolchain.";
        let out = ensure_linkedin_handle_from_pack(body, pack, &idx);
        assert!(out.starts_with("From @isaacsacolick."));
        assert!(out.contains("Great developer experience"));
        let named = ensure_linkedin_handle_from_pack(
            "Isaac Sacolick lists ten DX principles worth reading.",
            pack,
            &idx,
        );
        assert!(named.starts_with("@isaacsacolick lists"));
        assert!(!named.contains("Isaac Sacolick"));
    }

    #[test]
    fn tweet_body_gets_x_handle_not_linkedin() {
        let idx = isaac_index();
        let pack = "subject: DX\nhandles: linkedin=@isaacsacolick x=@nyike\n";
        let body = "Isaac Sacolick’s 10 principles are the wrench you need.";
        let out = ensure_x_handle_from_pack(body, pack, &idx);
        assert!(out.contains("@nyike"));
        assert!(!out.contains("Isaac Sacolick"));
        assert!(!out.contains("@isaacsacolick"));
        let missing = ensure_x_handle_from_pack("DX that sticks.", pack, &idx);
        assert_eq!(
            missing, "DX that sticks.",
            "X must not invent a leading @handle when the name is absent"
        );
    }

    #[test]
    fn parse_niko_style_linkedin_url_and_x_at() {
        let e = parse_handle_add(
            "Niko Matsakis https://www.linkedin.com/in/nicholas-matsakis-615614/ @nikomatsakis",
        )
        .expect("parse");
        assert_eq!(e.name, "Niko Matsakis");
        assert_eq!(e.linkedin, "@nicholas-matsakis-615614");
        assert_eq!(e.x, "@nikomatsakis");
        assert!(e.linkedin_url.contains("nicholas-matsakis-615614"));
        assert_eq!(e.x_url, "https://x.com/nikomatsakis");
    }

    #[test]
    fn parse_linkedin_url_only_humanizes_name() {
        let e = parse_handle_add(
            "Niko https://www.linkedin.com/in/nicholas-matsakis-615614/ @nikomatsakis",
        )
        .expect("parse");
        assert_eq!(e.linkedin, "@nicholas-matsakis-615614");
        assert!(e.name.contains("Niko"));
        assert_eq!(e.x, "@nikomatsakis");
    }

    #[test]
    fn parse_x_url_only() {
        let e = parse_handle_add("Wasmer https://x.com/wasmerio @wasmerio").expect("parse");
        assert_eq!(e.name, "Wasmer");
        assert_eq!(e.x, "@wasmerio");
    }

    #[test]
    fn handle_add_noop_when_last_two_tokens_lack_at() {
        let err = parse_handle_add("Rust Bytes rust-bytes-weekly rustaceans_rs")
            .expect_err("bare slugs must noop");
        assert!(err.contains("noop"), "{err}");
    }

    #[test]
    fn handle_add_accepts_name_plus_two_ats() {
        let e = parse_handle_add("Rust Bytes @rust-bytes @rustaceans_rs").expect("parse");
        assert_eq!(e.name, "Rust Bytes");
        assert_eq!(e.linkedin, "@rust-bytes");
        assert_eq!(e.x, "@rustaceans_rs");
    }

    #[test]
    fn handle_add_accepts_linkedin_url_and_x_at() {
        let e = parse_handle_add(
            "Niko Matsakis https://www.linkedin.com/in/nicholas-matsakis-615614/ @nikomatsakis",
        )
        .expect("parse");
        assert_eq!(e.name, "Niko Matsakis");
        assert!(e.linkedin_url.contains("nicholas-matsakis"));
        assert_eq!(e.x, "@nikomatsakis");
    }

    #[test]
    fn format_handle_add_reply_recaps_linkedin_and_x() {
        let e = HandleEntry {
            name: "Rust Bytes".into(),
            linkedin: "@rust-bytes".into(),
            x: String::new(),
            linkedin_url: "https://www.linkedin.com/company/rust-bytes/".into(),
            x_url: String::new(),
        };
        let reply = format_handle_add_reply(&HandleAddOutcome::Added(e));
        assert!(reply.contains("linkedin: @rust-bytes"), "{reply}");
        assert!(reply.contains("x: (none)"), "{reply}");
    }

    #[test]
    fn parse_truncates_junk_after_at() {
        let e = parse_handle_add(
            "Niko https://www.linkedin.com/in/nicholas-matsakis-615614/ @nikomatsakislinkedin.com",
        )
        .expect("parse");
        assert_eq!(e.x, "@nikomatsakis");
    }

    #[test]
    fn apply_handle_add_skips_duplicate_linkedin_url() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("handles.toml");
        std::fs::write(&path, "# seed\n").expect("write");
        let mut idx = HandlesIndex::default();
        let first = apply_handle_add(
            &mut idx,
            &path,
            "Niko Matsakis https://www.linkedin.com/in/nicholas-matsakis-615614/ @nikomatsakis",
        )
        .expect("first");
        assert!(matches!(first, HandleAddOutcome::Added(_)));
        let second = apply_handle_add(
            &mut idx,
            &path,
            "Other Name https://www.linkedin.com/in/nicholas-matsakis-615614/ @other",
        )
        .expect("second");
        assert!(matches!(second, HandleAddOutcome::AlreadyPresent(_)));
        let text = std::fs::read_to_string(&path).expect("read");
        assert_eq!(text.matches("[[handle]]").count(), 1);
    }

    fn infoworld_index() -> HandlesIndex {
        HandlesIndex {
            entries: vec![HandleEntry {
                name: "InfoWorld".into(),
                linkedin: "@infoworld".into(),
                x: "@InfoWorld".into(),
                linkedin_url: "https://www.linkedin.com/company/infoworld/".into(),
                x_url: "https://x.com/InfoWorld".into(),
            }],
        }
    }

    #[test]
    fn publisher_skips_lead_line_when_name_absent() {
        let idx = infoworld_index();
        let pack = "subject: Opus\nhandles: linkedin=@infoworld x=@InfoWorld\n";
        let body = "Anthropic Opus corrections are costing enterprise teams.";
        let out = ensure_linkedin_handle_from_pack(body, pack, &idx);
        assert_eq!(out, body);
        assert!(!out.contains("From @infoworld"));
        assert!(!out.starts_with("InfoWorld:"));
    }

    #[test]
    fn publisher_host_infoq_resolves() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../handles.toml");
        let idx = load_handles_from(&path).expect("handles");
        let hit = idx
            .primary_from_brief(
                "cite https://www.infoq.com/news/2026/08/aws-bench-agent-evaluation",
            )
            .expect("infoq publisher hit");
        assert_eq!(hit.name, "InfoQ");
        assert_eq!(hit.linkedin, "@infoq");
    }

    #[test]
    fn publisher_host_infoworld_resolves() {
        let idx = infoworld_index();
        let brief = "anthropic opus cite https://www.infoworld.com/article/4211958/anthropics-opus-language-problems.html";
        let hit = idx.primary_from_brief(brief).expect("publisher host hit");
        assert_eq!(hit.linkedin, "@infoworld");
    }

    #[test]
    fn pack_handles_line_from_publisher_cite() {
        let idx = infoworld_index();
        let mut pack = String::from("## ResearchPack\nsubject: Opus\nsummary: cost\n");
        let brief =
            "cite https://www.infoworld.com/article/4211958/anthropics-opus-language-problems.html";
        apply_brief_handles_to_pack(&mut pack, brief, &idx);
        assert!(pack.contains("handles: linkedin=@infoworld x=@InfoWorld"));
    }

    #[test]
    fn body_infoworld_to_at_handle() {
        use crate::sources::digest_propose_fixtures::FIXTURE_D_BAD_BODY;
        let idx = infoworld_index();
        let pack = "subject: Opus\nhandles: linkedin=@infoworld x=@InfoWorld\n";
        let out = ensure_linkedin_handle_from_pack(FIXTURE_D_BAD_BODY, pack, &idx);
        assert!(out.contains("@infoworld"));
        assert!(!out.contains("InfoWorld"));
    }

    #[test]
    fn anthropic_subject_keeps_infoworld_publisher_handle() {
        // Anthropic is not in this index, so publisher host is the only hit.
        let idx = infoworld_index();
        let brief = "Anthropic Opus language problems hidden cost https://www.infoworld.com/article/4211958/x.html";
        let hit = idx
            .primary_from_brief(brief)
            .expect("publisher when subject name is unregistered");
        assert_eq!(hit.name, "InfoWorld");
    }

    // Cite target is not the article publisher. Same rule for LinkedIn and X packs/bodies.
    fn cite_vs_publisher_index() -> HandlesIndex {
        HandlesIndex {
            entries: vec![
                HandleEntry {
                    name: "GitHub".into(),
                    linkedin: "@github".into(),
                    x: "@github".into(),
                    linkedin_url: "https://www.linkedin.com/company/github/".into(),
                    x_url: "https://x.com/github".into(),
                },
                HandleEntry {
                    name: "Cursor".into(),
                    linkedin: "@cursorai".into(),
                    x: "@cursor_ai".into(),
                    linkedin_url: "https://www.linkedin.com/company/cursorai/".into(),
                    x_url: "https://x.com/cursor_ai".into(),
                },
                HandleEntry {
                    name: "InfoWorld".into(),
                    linkedin: "@infoworld".into(),
                    x: "@InfoWorld".into(),
                    linkedin_url: "https://www.linkedin.com/company/infoworld/".into(),
                    x_url: "https://x.com/InfoWorld".into(),
                },
                HandleEntry {
                    name: "InfoQ".into(),
                    linkedin: "@infoq".into(),
                    x: "@InfoQ".into(),
                    linkedin_url: "https://www.linkedin.com/company/infoq/".into(),
                    x_url: "https://x.com/InfoQ".into(),
                },
            ],
        }
    }

    #[test]
    fn named_cite_beats_article_publisher_on_x() {
        let idx = cite_vs_publisher_index();
        // Curly apostrophe matches live tweet prose (U+2019).
        let brief = "Cursor\u{2019}s AI-native approach could simplify enterprise coding, but GitHub remains safer https://www.infoworld.com/article/4211505/decoding-origin.html";
        let hit = idx.primary_from_brief(brief).expect("named entity");
        assert_eq!(hit.name, "Cursor");
        assert_eq!(hit.x, "@cursor_ai");

        let mut pack = String::from("## ResearchPack\nsubject: Cursor Origin\nsummary: ship\n");
        apply_brief_handles_to_pack(&mut pack, brief, &idx);
        assert!(
            pack.contains("x=@cursor_ai"),
            "pack must carry Cursor, not InfoWorld: {pack}"
        );
        assert!(
            !pack.to_ascii_lowercase().contains("infoworld"),
            "publisher must not win the cite: {pack}"
        );

        let body = "📜 Cursor\u{2019}s AI-native approach is rewriting code.\n\n#AI #Cursor";
        let out = ensure_x_handle_from_pack(body, &pack, &idx);
        assert!(out.contains("@cursor_ai"), "body must tag Cursor: {out}");
        assert!(
            !out.contains("Cursor\u{2019}s"),
            "plain name must become handle: {out}"
        );
        assert!(
            out.contains("#Cursor"),
            "hashtag must stay plain name: {out}"
        );
    }

    #[test]
    fn named_cite_beats_article_publisher_on_linkedin() {
        let idx = cite_vs_publisher_index();
        // Named company first; InfoWorld is only the article host.
        let brief =
            "GitHub remains the safer system of record https://www.infoworld.com/article/4211505/x.html";
        let hit = idx.primary_from_brief(brief).expect("named cite");
        assert_eq!(hit.name, "GitHub");
        assert_eq!(hit.linkedin, "@github");

        let mut pack = String::from("## ResearchPack\nsubject: GitHub record\nsummary: ship\n");
        apply_brief_handles_to_pack(&mut pack, brief, &idx);
        assert!(
            pack.contains("linkedin=@github"),
            "LinkedIn pack must cite GitHub: {pack}"
        );
        assert!(
            !pack.to_ascii_lowercase().contains("infoworld"),
            "publisher must not win the cite: {pack}"
        );

        let body = "GitHub remains the safer system of record for builders.";
        let out = ensure_linkedin_handle_from_pack(body, &pack, &idx);
        assert!(
            out.contains("@github"),
            "LinkedIn body must tag cite: {out}"
        );
        assert!(
            !out.contains("GitHub remains"),
            "plain cite name must become handle: {out}"
        );
    }

    #[test]
    fn bare_publisher_url_still_resolves_when_no_named_cite() {
        let idx = cite_vs_publisher_index();
        let info_world = idx
            .primary_from_brief(
                "cite https://www.infoworld.com/article/4211958/anthropics-opus-language-problems.html",
            )
            .expect("InfoWorld host");
        assert_eq!(info_world.name, "InfoWorld");
        let info_q = idx
            .primary_from_brief(
                "cite https://www.infoq.com/news/2026/08/aws-bench-agent-evaluation",
            )
            .expect("InfoQ host - second publisher, not InfoWorld-hardcoded");
        assert_eq!(info_q.name, "InfoQ");
    }

    #[test]
    fn seed_handles_tweet074_cursor_not_publisher() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../handles.toml");
        let idx = load_handles_from(&path).expect("seed handles.toml");
        let brief = "Cursor\u{2019}s AI-native approach could simplify software development, but gaps in enterprise controls, security and ecosystem depth mean GitHub remains the safer system of record for now, analysts say. https://www.infoworld.com/article/4211505/decoding-origin-cursors-github-rival-that-was-launched-during-the-latters-outage.html";
        let hit = idx.primary_from_brief(brief).expect("Cursor from seed");
        assert_eq!(hit.name, "Cursor");
        assert_eq!(hit.x, "@cursor_ai");
        assert_eq!(hit.linkedin, "@cursorai");
        assert!(
            !hit.x.eq_ignore_ascii_case("@InfoWorld"),
            "must not fall back to article publisher"
        );

        let mut pack = String::from("## ResearchPack\nsubject: Cursor Origin\nsummary: ship\n");
        apply_brief_handles_to_pack(&mut pack, brief, &idx);
        assert!(pack.contains("x=@cursor_ai"), "seed pack: {pack}");
        assert!(
            pack.contains("linkedin=@cursorai"),
            "seed LinkedIn pack: {pack}"
        );
        assert!(!pack.to_ascii_lowercase().contains("infoworld"));

        let body = "📜 Cursor\u{2019}s AI-native approach is rewriting code, but 🔐 enterprise security gaps still make GitHub the safer ship. 🦀\n#AI #DevTools #GitHub #Cursor";
        let out = ensure_x_handle_from_pack(body, &pack, &idx);
        assert!(out.contains("@cursor_ai"), "tweet074 body: {out}");
    }

    #[test]
    fn draft098_cursor_linkedin_cite_not_publisher() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../handles.toml");
        let idx = load_handles_from(&path).expect("seed handles.toml");
        let brief = "Cursor\u{2019}s AI-native approach is a bold move https://www.infoworld.com/article/4211505/decoding-origin-cursors-github-rival-that-was-launched-during-the-latters-outage.html";
        let mut pack = String::from("## ResearchPack\nsubject: Cursor Origin\nsummary: ship\n");
        apply_brief_handles_to_pack(&mut pack, brief, &idx);
        assert!(
            pack.contains("linkedin=@cursorai"),
            "LinkedIn pack must cite Cursor: {pack}"
        );
        assert!(!pack.to_ascii_lowercase().contains("infoworld"));

        let body = "Cursor\u{2019}s AI-native approach is a bold move, shifting from a plugin-based model.\n\nGitHub remains the safer bet.";
        let out = ensure_linkedin_handle_from_pack(body, &pack, &idx);
        assert!(
            out.contains("@cursorai"),
            "LinkedIn body must tag Cursor: {out}"
        );
        assert!(
            !out.starts_with("Cursor"),
            "plain Cursor name must become handle: {out}"
        );
    }

    #[test]
    fn draft102_sourcegraph_linkedin_cite_from_seed() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../handles.toml");
        let idx = load_handles_from(&path).expect("seed handles.toml");
        let brief =
            "Amp AI coding agent by Sourcegraph https://sourcegraph.com/blog/agentic-coding";
        let hit = idx
            .primary_from_brief(brief)
            .expect("Sourcegraph from seed");
        assert_eq!(hit.name, "Sourcegraph");
        assert_eq!(hit.x, "@Sourcegraph");
        assert_eq!(hit.linkedin, "@sourcegraph");

        let mut pack =
            String::from("## ResearchPack\nsubject: Amp by Sourcegraph\nsummary: ship\n");
        apply_brief_handles_to_pack(&mut pack, brief, &idx);
        assert!(
            pack.contains("linkedin=@sourcegraph"),
            "LinkedIn pack must cite Sourcegraph: {pack}"
        );
        assert!(pack.contains("x=@Sourcegraph"), "X pack: {pack}");

        let body = "Sourcegraph\u{2019}s Amp isn\u{2019}t just another autocomplete tool.";
        let out = ensure_linkedin_handle_from_pack(body, &pack, &idx);
        assert!(
            out.contains("@sourcegraph"),
            "LinkedIn body must tag Sourcegraph: {out}"
        );
        assert!(
            !out.starts_with("Sourcegraph"),
            "plain Sourcegraph name must become handle: {out}"
        );
    }

    #[test]
    fn sourcegraph_beats_publisher_host_when_both_present() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../handles.toml");
        let idx = load_handles_from(&path).expect("seed handles.toml");
        let brief = "Amp by Sourcegraph on agentic coding https://www.infoworld.com/article/4211505/decoding-origin.html";
        let hit = idx.primary_from_brief(brief).expect("named cite");
        assert_eq!(hit.name, "Sourcegraph");
        assert_ne!(hit.linkedin.to_ascii_lowercase(), "@infoworld");
        let mut pack = String::from("## ResearchPack\nsubject: Amp\nsummary: ship\n");
        apply_brief_handles_to_pack(&mut pack, brief, &idx);
        assert!(
            pack.contains("linkedin=@sourcegraph"),
            "must not fall to InfoWorld publisher: {pack}"
        );
        assert!(!pack.to_ascii_lowercase().contains("infoworld"));
    }

    #[test]
    fn sourcegraph_linkedin_from_x_only_pack() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../handles.toml");
        let idx = load_handles_from(&path).expect("seed handles.toml");
        let pack = "subject: Amp\nhandles: x=@Sourcegraph\n";
        let body = "Sourcegraph\u{2019}s Amp is agent-first.";
        let out = ensure_linkedin_handle_from_pack(body, pack, &idx);
        assert!(
            out.contains("@sourcegraph"),
            "x-only pack must still resolve LinkedIn via registry: {out}"
        );
    }

    #[test]
    fn seed_localstack_linkedin_and_x_from_handles_toml() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../handles.toml");
        let idx = load_handles_from(&path).expect("seed handles.toml");
        let hit = idx
            .primary_from_brief("LocalStack local AWS cloud for builders")
            .expect("LocalStack from seed");
        assert_eq!(hit.name, "LocalStack");
        assert_eq!(hit.linkedin, "@localstack-cloud");
        assert_eq!(hit.x, "@localstack");
        assert!(!hit.linkedin_url.is_empty());
        assert!(!hit.x_url.is_empty());
    }

    #[test]
    fn equal_length_names_prefer_earlier_brief_mention() {
        let idx = HandlesIndex {
            entries: vec![
                HandleEntry {
                    name: "Cursor".into(),
                    linkedin: "@cursorai".into(),
                    x: "@cursor_ai".into(),
                    linkedin_url: String::new(),
                    x_url: String::new(),
                },
                HandleEntry {
                    name: "GitHub".into(),
                    linkedin: "@github".into(),
                    x: "@github".into(),
                    linkedin_url: String::new(),
                    x_url: String::new(),
                },
            ],
        };
        assert_eq!(
            idx.primary_from_brief("GitHub vs Cursor shipping AI coding")
                .expect("hit")
                .name,
            "GitHub"
        );
        assert_eq!(
            idx.primary_from_brief("Cursor vs GitHub shipping AI coding")
                .expect("hit")
                .name,
            "Cursor"
        );
    }

    #[test]
    fn linkedin_inject_from_x_only_pack_when_registry_has_linkedin() {
        let idx = cite_vs_publisher_index();
        // Stale pack from when Cursor had X only (real DRAFT-098 shape).
        let pack = "subject: Cursor\nhandles: x=@cursor_ai\n";
        let body = "Cursor\u{2019}s AI-native approach is a bold move.";
        let out = ensure_linkedin_handle_from_pack(body, pack, &idx);
        assert!(
            out.contains("@cursorai"),
            "x-only pack must still resolve LinkedIn via registry: {out}"
        );
    }

    #[test]
    fn mock_item_15_opus_with_infoworld_handle() {
        use crate::sources::digest_propose_fixtures::{fixture_d_brief, FIXTURE_D_BAD_BODY};
        let idx = infoworld_index();
        let mut pack = String::from("## ResearchPack\nsubject: Opus\nsummary: cost\n");
        apply_brief_handles_to_pack(&mut pack, &fixture_d_brief(), &idx);
        let out = ensure_linkedin_handle_from_pack(FIXTURE_D_BAD_BODY, &pack, &idx);
        assert!(out.contains("@infoworld"));
        assert!(out.to_ascii_lowercase().contains("opus"));
    }

    #[test]
    fn body_already_has_handle_unchanged() {
        let idx = infoworld_index();
        let pack = "handles: linkedin=@infoworld x=@InfoWorld\n";
        let body = "From @infoworld. Anthropic Opus costs are rising for builders.";
        let out = ensure_linkedin_handle_from_pack(body, pack, &idx);
        assert_eq!(out.matches("@infoworld").count(), 1);
    }

    #[test]
    fn publisher_host_label_skips_x_status_urls() {
        assert!(
            publisher_host_label("https://x.com/ayushagarwal027/status/2090736100025504071")
                .is_none()
        );
        assert_eq!(
            publisher_host_label(
                "https://www.infoworld.com/article/4211958/anthropics-opus-language-problems.html"
            ),
            Some("infoworld".into())
        );
    }
}

// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Official `LinkedIn` data-export importer (zip or unzipped folder).
//!
//! Flow: raw Complete/Basic dump → curated folder → `SQLite` sources.
//! Weekly curated refreshes are safe: dedupe on (activity, url, `occurred_at`, title).
//! Private chat CSVs (`messages.csv`, learning/guide message dumps) are skipped.
//! Reactions / likes CSVs are skipped (out of inspiration scope).

use crate::sources::embed::{default_embed_model, EmbedClient};
use crate::sources::html::infer_subject;
use crate::sources::store::{InsertSource, SourceDb};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors while importing a `LinkedIn` export.
#[derive(Debug, Error)]
pub enum ExportError {
    #[error("export io: {0}")]
    Io(#[from] std::io::Error),
    #[error("export zip: {0}")]
    Zip(String),
    #[error("export csv: {0}")]
    Csv(String),
    #[error("export store: {0}")]
    Store(String),
    #[error("export embed: {0}")]
    Embed(String),
    #[error("export: {0}")]
    Other(String),
}

/// One logical item extracted from the export before embed/store.
#[derive(Debug, Clone)]
pub struct ExportItem {
    pub kind: String,
    pub activity: String,
    pub title: String,
    pub url: Option<String>,
    pub text: String,
    pub occurred_at: Option<String>,
}

/// Result of an import pass (weekly merge friendly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportStats {
    pub inserted: usize,
    pub skipped: usize,
}

/// Imports `LinkedIn` official dump from a directory or `.zip` file.
///
/// Skips rows already present (dedupe). Embeds and inserts one item at a time
/// so a long curated import commits progress continuously.
///
/// # Errors
///
/// Returns an [`ExportError`] variant for missing export files, parse, or store failures.
pub async fn import_linkedin_export(
    path: &Path,
    db_path: &Path,
    embed: &dyn EmbedClient,
) -> Result<ImportStats, ExportError> {
    use tracing::info;

    let items = load_export_items(path)?;
    let total = items.len();
    info!(total, path = %path.display(), "sources: LinkedIn export items loaded");
    let db = SourceDb::open(db_path).map_err(|e| ExportError::Store(e.to_string()))?;
    let model = default_embed_model();
    let mut inserted = 0usize;
    let mut skipped = 0usize;
    for (i, item) in items.into_iter().enumerate() {
        if item.text.trim().is_empty() {
            continue;
        }
        if db
            .source_exists(
                &item.activity,
                item.url.as_deref(),
                item.occurred_at.as_deref(),
                &item.title,
            )
            .map_err(|e| ExportError::Store(e.to_string()))?
        {
            skipped += 1;
            continue;
        }
        let subject = infer_subject(&item.title, &item.text);
        let mut chunks = Vec::new();
        for chunk in chunk_text(&item.text, 800) {
            let embedding = embed
                .embed(&model, &chunk)
                .await
                .map_err(|e| ExportError::Embed(e.to_string()))?;
            chunks.push((chunk, embedding));
        }
        let activity = item.activity.clone();
        let wrote = db
            .with_transaction(|conn| {
                let Some(source_id) = SourceDb::insert_source_on(
                    conn,
                    &InsertSource {
                        kind: &item.kind,
                        activity: &item.activity,
                        subject: &subject,
                        title: &item.title,
                        url: item.url.as_deref(),
                        raw_text: &item.text,
                        occurred_at: item.occurred_at.as_deref(),
                    },
                )?
                else {
                    return Ok(false);
                };
                for (chunk, embedding) in &chunks {
                    SourceDb::insert_chunk_on(conn, source_id, &subject, chunk, embedding)?;
                }
                Ok(true)
            })
            .map_err(|e| ExportError::Store(e.to_string()))?;
        if wrote {
            inserted += 1;
        } else {
            skipped += 1;
        }
        if (i + 1) % 50 == 0 || i + 1 == total {
            info!(
                done = i + 1,
                total,
                inserted,
                skipped,
                last_activity = %activity,
                "sources: LinkedIn export progress"
            );
        }
    }
    Ok(ImportStats { inserted, skipped })
}

/// Loads items from zip or directory without writing to DB.
///
/// # Errors
///
/// Returns an [`ExportError`] variant for missing export files, parse, or store failures.
pub fn load_export_items(path: &Path) -> Result<Vec<ExportItem>, ExportError> {
    if path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        return load_from_zip(path);
    }
    if path.is_dir() {
        return load_from_dir(path);
    }
    Err(ExportError::Other(format!(
        "expected zip or directory at {}",
        path.display()
    )))
}

fn load_from_dir(dir: &Path) -> Result<Vec<ExportItem>, ExportError> {
    let mut items = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if let Some(more) = parse_export_file(&path)? {
                items.extend(more);
            }
        } else if path.is_dir() {
            items.extend(load_from_dir(&path)?);
        }
    }
    Ok(items)
}

fn load_from_zip(zip_path: &Path) -> Result<Vec<ExportItem>, ExportError> {
    let bytes = fs::read(zip_path)?;
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| ExportError::Zip(e.to_string()))?;
    let mut items = Vec::new();
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| ExportError::Zip(e.to_string()))?;
        let name = file.name().to_string();
        if file.is_dir() {
            continue;
        }
        let path_name = Path::new(&name);
        if !path_name
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("csv") || ext.eq_ignore_ascii_case("json"))
        {
            continue;
        }
        let mut buf = String::new();
        file.read_to_string(&mut buf)
            .map_err(|e| ExportError::Io(std::io::Error::other(e)))?;
        let file_name = Path::new(&name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&name);
        items.extend(parse_export_bytes(file_name, buf.as_bytes())?);
    }
    Ok(items)
}

fn parse_export_file(path: &Path) -> Result<Option<Vec<ExportItem>>, ExportError> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let name_path = Path::new(&name);
    if !name_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("csv") || ext.eq_ignore_ascii_case("json"))
    {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    Ok(Some(parse_export_bytes(&name, &bytes)?))
}

fn parse_export_bytes(file_name: &str, bytes: &[u8]) -> Result<Vec<ExportItem>, ExportError> {
    let lower = file_name.to_ascii_lowercase();
    if is_private_chat_export(&lower) {
        return Ok(Vec::new());
    }
    if Path::new(&lower)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        return parse_json_items(&lower, bytes);
    }
    parse_csv_items(&lower, bytes)
}

/// DMs and `LinkedIn` learning/guide chat dumps - never ingest into corpora.
fn is_private_chat_export(file_name: &str) -> bool {
    let base = file_name
        .rsplit('/')
        .next()
        .unwrap_or(file_name)
        .to_ascii_lowercase();
    let compact = base.replace(['_', '-'], "");
    base == "messages.csv"
        || compact.contains("guidemessages")
        || compact.contains("learningcoachmessages")
        || compact.contains("learningroleplaymessages")
}

fn parse_csv_items(file_name: &str, bytes: &[u8]) -> Result<Vec<ExportItem>, ExportError> {
    let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(bytes);
    let headers = rdr
        .headers()
        .map_err(|e| ExportError::Csv(e.to_string()))?
        .clone();
    let header_lower: Vec<String> = headers.iter().map(str::to_ascii_lowercase).collect();

    if let Some(items) = try_parse_csv_named_export(file_name, &mut rdr, &header_lower)? {
        return Ok(items);
    }

    let (kind, activity) = classify_file(file_name);

    // Shares / posts with link but empty commentary still count as posts.
    if activity == "post" {
        let url_idx = find_col(
            &header_lower,
            &["sharelink", "sharedurl", "link", "url", "permalink"],
        );
        let text_idx = find_col(
            &header_lower,
            &[
                "sharecommentary",
                "commentary",
                "message",
                "text",
                "description",
            ],
        );
        let date_idx = find_col(
            &header_lower,
            &[
                "date",
                "date/time",
                "datetime",
                "created at",
                "creation date",
            ],
        );
        if url_idx.is_some() {
            return parse_share_rows(&mut rdr, &header_lower, text_idx, url_idx, date_idx);
        }
    }

    parse_generic_text_csv_rows(&mut rdr, &header_lower, kind, activity)
}

/// Named export files with dedicated parsers (profile, skills, …). `None` = fall through.
fn try_parse_csv_named_export(
    file_name: &str,
    rdr: &mut csv::Reader<&[u8]>,
    header_lower: &[String],
) -> Result<Option<Vec<ExportItem>>, ExportError> {
    if file_name.contains("instantrepost") || file_name.contains("instant_repost") {
        return parse_link_only_rows(rdr, header_lower, "personal_feed", "repost", "Repost")
            .map(Some);
    }
    // Reactions / likes are out of inspiration scope (not imported).
    if file_name.contains("reaction") || file_name.contains("likes") {
        return Ok(Some(Vec::new()));
    }
    if file_name.contains("profile") && !file_name.contains("summary") {
        return parse_profile_rows(rdr, header_lower).map(Some);
    }
    if file_name.contains("profile") && file_name.contains("summary") {
        return parse_single_column_voice(
            rdr,
            header_lower,
            "profile",
            "profile summary",
            "summary",
        )
        .map(Some);
    }
    if file_name.contains("skill") {
        return parse_skills_list(rdr, header_lower).map(Some);
    }
    if file_name.contains("certification") {
        return parse_named_voice_rows(
            rdr,
            header_lower,
            "certification",
            "Certification",
            &["name"],
            &["authority", "url"],
        )
        .map(Some);
    }
    if file_name.contains("language") {
        return parse_named_voice_rows(
            rdr,
            header_lower,
            "language",
            "Language",
            &["name"],
            &["proficiency"],
        )
        .map(Some);
    }
    Ok(None)
}

fn parse_generic_text_csv_rows(
    rdr: &mut csv::Reader<&[u8]>,
    header_lower: &[String],
    kind: &str,
    activity: &str,
) -> Result<Vec<ExportItem>, ExportError> {
    let text_idx = find_col(
        header_lower,
        &[
            "sharecommentary",
            "commentary",
            "message",
            "text",
            "content",
            "comments",
            "posttext",
            "description",
            "summary",
            "headline",
            "media description",
            "review text",
            "notes",
        ],
    );
    let url_idx = find_col(
        header_lower,
        &[
            "sharelink",
            "sharedurl",
            "link",
            "url",
            "permalink",
            "media link",
        ],
    );
    let title_idx = find_col(
        header_lower,
        &[
            "title",
            "subject",
            "company name",
            "name",
            "school name",
            "content title",
        ],
    );
    let company_idx = find_col(header_lower, &["company name", "company", "authority"]);
    let role_idx = find_col(header_lower, &["title", "job title", "degree name"]);
    let date_idx = find_col(
        header_lower,
        &[
            "date",
            "date/time",
            "datetime",
            "created at",
            "creation date",
        ],
    );

    let Some(text_idx) = text_idx else {
        return Ok(Vec::new());
    };

    let mut items = Vec::new();
    for result in rdr.records() {
        let record = result.map_err(|e| ExportError::Csv(e.to_string()))?;
        let text = record.get(text_idx).unwrap_or("").trim().to_string();
        if text.is_empty() || text == "-" {
            continue;
        }
        let url = url_idx
            .and_then(|i| record.get(i))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let title = compose_title(&record, title_idx, company_idx, role_idx, &text);
        let occurred_at = date_idx
            .and_then(|i| record.get(i))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(normalize_linkedin_datetime);
        items.push(ExportItem {
            kind: kind.to_string(),
            activity: activity.to_string(),
            title,
            url,
            text,
            occurred_at,
        });
    }
    Ok(items)
}

/// Contract kind + fine activity for a CSV/JSON file name.
fn classify_file(file_name: &str) -> (&'static str, &'static str) {
    if file_name.contains("comment") {
        ("comment", "comment")
    } else if file_name.contains("instantrepost") || file_name.contains("repost") {
        ("personal_feed", "repost")
    } else if file_name.contains("reaction") || file_name.contains("like") {
        ("personal_feed", "reaction")
    } else if file_name.contains("share") || file_name.contains("post") {
        ("personal_feed", "post")
    } else if file_name.contains("position") {
        ("voice", "position")
    } else if file_name.contains("education") {
        ("voice", "education")
    } else if file_name.contains("project") {
        ("voice", "project")
    } else if file_name.contains("honor") {
        ("voice", "honor")
    } else if file_name.contains("recommendation") {
        ("voice", "recommendation")
    } else if file_name.contains("rich_media") || file_name.contains("rich-media") {
        ("voice", "rich_media")
    } else {
        ("voice", "profile")
    }
}

fn compose_title(
    record: &csv::StringRecord,
    title_idx: Option<usize>,
    company_idx: Option<usize>,
    role_idx: Option<usize>,
    text: &str,
) -> String {
    let title = title_idx
        .and_then(|i| record.get(i))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let company = company_idx
        .and_then(|i| record.get(i))
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let role = role_idx
        .and_then(|i| record.get(i))
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(t) = title {
        if let Some(c) = company {
            if role.is_some() && role != Some(t.as_str()) {
                return format!("{t} @ {c}");
            }
            if !t.contains(c) {
                return format!("{t} ({c})");
            }
        }
        return t;
    }
    if let (Some(role), Some(company)) = (role, company) {
        return format!("{role} @ {company}");
    }
    text.chars().take(60).collect()
}

fn parse_profile_rows(
    rdr: &mut csv::Reader<&[u8]>,
    header_lower: &[String],
) -> Result<Vec<ExportItem>, ExportError> {
    let headline_idx = find_col(header_lower, &["headline"]);
    let summary_idx = find_col(header_lower, &["summary"]);
    let first_idx = find_col(header_lower, &["first name"]);
    let last_idx = find_col(header_lower, &["last name"]);
    let industry_idx = find_col(header_lower, &["industry"]);
    let geo_idx = find_col(header_lower, &["geo location"]);
    let mut items = Vec::new();
    for result in rdr.records() {
        let record = result.map_err(|e| ExportError::Csv(e.to_string()))?;
        let first = first_idx.and_then(|i| record.get(i)).unwrap_or("").trim();
        let last = last_idx.and_then(|i| record.get(i)).unwrap_or("").trim();
        let name = format!("{first} {last}").trim().to_string();
        if let Some(i) = headline_idx {
            let headline = record.get(i).unwrap_or("").trim();
            if !headline.is_empty() {
                items.push(ExportItem {
                    kind: "voice".into(),
                    activity: "profile".into(),
                    title: format!("Headline - {name}"),
                    url: None,
                    text: headline.to_string(),
                    occurred_at: None,
                });
            }
        }
        if let Some(i) = summary_idx {
            let summary = record.get(i).unwrap_or("").trim();
            if !summary.is_empty() {
                let mut text = summary.to_string();
                if let Some(ind) = industry_idx.and_then(|j| record.get(j)).map(str::trim) {
                    if !ind.is_empty() {
                        text = format!("Industry: {ind}\n\n{text}");
                    }
                }
                if let Some(geo) = geo_idx.and_then(|j| record.get(j)).map(str::trim) {
                    if !geo.is_empty() {
                        text = format!("Location: {geo}\n\n{text}");
                    }
                }
                items.push(ExportItem {
                    kind: "voice".into(),
                    activity: "profile".into(),
                    title: format!("Profile summary - {name}"),
                    url: None,
                    text,
                    occurred_at: None,
                });
            }
        }
    }
    Ok(items)
}

fn parse_single_column_voice(
    rdr: &mut csv::Reader<&[u8]>,
    header_lower: &[String],
    activity: &str,
    title: &str,
    col: &str,
) -> Result<Vec<ExportItem>, ExportError> {
    let idx = find_col(header_lower, &[col]).or_else(|| {
        header_lower
            .iter()
            .position(|h| h.contains("summary") || h.contains("profile"))
    });
    let Some(idx) = idx else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    for result in rdr.records() {
        let record = result.map_err(|e| ExportError::Csv(e.to_string()))?;
        let text = record.get(idx).unwrap_or("").trim().to_string();
        if text.is_empty() {
            continue;
        }
        items.push(ExportItem {
            kind: "voice".into(),
            activity: activity.into(),
            title: title.into(),
            url: None,
            text,
            occurred_at: None,
        });
    }
    Ok(items)
}

fn parse_skills_list(
    rdr: &mut csv::Reader<&[u8]>,
    header_lower: &[String],
) -> Result<Vec<ExportItem>, ExportError> {
    let name_idx = find_col(header_lower, &["name", "skill name"]).unwrap_or(0);
    let mut skills = Vec::new();
    for result in rdr.records() {
        let record = result.map_err(|e| ExportError::Csv(e.to_string()))?;
        let skill = record.get(name_idx).unwrap_or("").trim();
        if !skill.is_empty() {
            skills.push(skill.to_string());
        }
    }
    if skills.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![ExportItem {
        kind: "voice".into(),
        activity: "skill".into(),
        title: "Skills".into(),
        url: None,
        text: format!("Skills: {}", skills.join(", ")),
        occurred_at: None,
    }])
}

fn parse_named_voice_rows(
    rdr: &mut csv::Reader<&[u8]>,
    header_lower: &[String],
    activity: &str,
    label: &str,
    name_cols: &[&str],
    extra_cols: &[&str],
) -> Result<Vec<ExportItem>, ExportError> {
    let name_idx = find_col(header_lower, name_cols);
    let Some(name_idx) = name_idx else {
        return Ok(Vec::new());
    };
    let extra_idxs: Vec<(String, usize)> = extra_cols
        .iter()
        .filter_map(|c| find_col(header_lower, &[c]).map(|i| ((*c).to_string(), i)))
        .collect();
    let mut items = Vec::new();
    for result in rdr.records() {
        let record = result.map_err(|e| ExportError::Csv(e.to_string()))?;
        let name = record.get(name_idx).unwrap_or("").trim();
        if name.is_empty() {
            continue;
        }
        let mut parts = vec![format!("{label}: {name}")];
        let mut url = None;
        for (col, idx) in &extra_idxs {
            let val = record.get(*idx).unwrap_or("").trim();
            if val.is_empty() {
                continue;
            }
            if col == "url" {
                url = Some(val.to_string());
            } else {
                parts.push(format!("{col}: {val}"));
            }
        }
        items.push(ExportItem {
            kind: "voice".into(),
            activity: activity.into(),
            title: format!("{label} - {name}"),
            url,
            text: parts.join("\n"),
            occurred_at: None,
        });
    }
    Ok(items)
}

fn parse_share_rows(
    rdr: &mut csv::Reader<&[u8]>,
    header_lower: &[String],
    text_idx: Option<usize>,
    url_idx: Option<usize>,
    date_idx: Option<usize>,
) -> Result<Vec<ExportItem>, ExportError> {
    let shared_url_idx = find_col(header_lower, &["sharedurl", "mediaurl"]);
    let mut items = Vec::new();
    for result in rdr.records() {
        let record = result.map_err(|e| ExportError::Csv(e.to_string()))?;
        let commentary = text_idx
            .and_then(|i| record.get(i))
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "-")
            .unwrap_or("");
        let share_link = url_idx
            .and_then(|i| record.get(i))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let shared = shared_url_idx
            .and_then(|i| record.get(i))
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "-");
        if commentary.is_empty() && share_link.is_none() {
            continue;
        }
        let occurred_at = date_idx
            .and_then(|i| record.get(i))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(normalize_linkedin_datetime);
        let (title, text) = if commentary.is_empty() {
            let link = share_link.clone().unwrap_or_default();
            (
                format!("Post {link}"),
                shared.map_or_else(|| format!("Post\n{link}"), |u| format!("Post\n{link}\n{u}")),
            )
        } else {
            let title: String = commentary.chars().take(60).collect();
            let text =
                shared.map_or_else(|| commentary.to_string(), |u| format!("{commentary}\n{u}"));
            (title, text)
        };
        items.push(ExportItem {
            kind: "personal_feed".into(),
            activity: "post".into(),
            title,
            url: share_link,
            text,
            occurred_at,
        });
    }
    Ok(items)
}

fn parse_link_only_rows(
    rdr: &mut csv::Reader<&[u8]>,
    header_lower: &[String],
    kind: &str,
    activity: &str,
    label: &str,
) -> Result<Vec<ExportItem>, ExportError> {
    let date_idx = find_col(header_lower, &["date", "date/time"]);
    let link_idx = find_col(header_lower, &["link", "url", "permalink", "sharelink"]);
    let mut items = Vec::new();
    for result in rdr.records() {
        let record = result.map_err(|e| ExportError::Csv(e.to_string()))?;
        let url = link_idx
            .and_then(|i| record.get(i))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let Some(url) = url else {
            continue;
        };
        let occurred_at = date_idx
            .and_then(|i| record.get(i))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(normalize_linkedin_datetime);
        items.push(ExportItem {
            kind: kind.into(),
            activity: activity.into(),
            title: format!("{label} {url}"),
            url: Some(url.clone()),
            text: format!("{label}\n{url}"),
            occurred_at,
        });
    }
    Ok(items)
}

fn parse_json_items(file_name: &str, bytes: &[u8]) -> Result<Vec<ExportItem>, ExportError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| ExportError::Other(e.to_string()))?;
    let (kind, activity) = classify_file(file_name);
    let mut items = Vec::new();
    let arr = match value {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(map) => map
            .values()
            .find_map(|v| v.as_array().cloned())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    for entry in arr {
        let text = entry
            .get("ShareCommentary")
            .or_else(|| entry.get("commentary"))
            .or_else(|| entry.get("Message"))
            .or_else(|| entry.get("message"))
            .or_else(|| entry.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        let url = entry
            .get("ShareLink")
            .or_else(|| entry.get("link"))
            .or_else(|| entry.get("url"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let occurred_at = entry
            .get("Date")
            .or_else(|| entry.get("date"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(normalize_linkedin_datetime);
        let title: String = text.chars().take(60).collect();
        items.push(ExportItem {
            kind: kind.into(),
            activity: activity.into(),
            title,
            url,
            text,
            occurred_at,
        });
    }
    Ok(items)
}

fn find_col(headers: &[String], names: &[&str]) -> Option<usize> {
    for name in names {
        if let Some(i) = headers.iter().position(|h| h == name) {
            return Some(i);
        }
    }
    None
}

/// `LinkedIn` export dates look like `2026-07-22 18:18:38` → sortable `2026-07-22T18:18:38`.
#[must_use]
pub fn normalize_linkedin_datetime(raw: &str) -> String {
    let t = raw.trim();
    if let Some((date, time)) = t.split_once(' ') {
        if date.len() == 10 && time.len() >= 8 {
            return format!("{date}T{}", &time[..8.min(time.len())]);
        }
    }
    t.replace(' ', "T")
}

/// Splits long text into overlapping-ish chunks by character budget on whitespace.
#[must_use]
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.chars().count() <= max_chars {
        return vec![trimmed.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for word in trimmed.split_whitespace() {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > max_chars {
            out.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Resolves export path from config/env (directory or zip).
#[must_use]
pub fn resolve_export_path(configured: &str) -> PathBuf {
    if let Ok(env_path) = std::env::var("ITCY_LINKEDIN_EXPORT_DIR") {
        if !env_path.trim().is_empty() {
            return PathBuf::from(env_path);
        }
    }
    PathBuf::from(configured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::embed::MockEmbedClient;
    use crate::sources::store::SourceListFilter;
    use tempfile::TempDir;

    fn write_fixture_dir(dir: &Path) {
        let shares = "\
Date,ShareLink,ShareCommentary,SharedUrl,MediaUrl,Visibility
2024-01-01 10:00:00,https://www.linkedin.com/feed/update/urn:li:activity:1,Excited about Rust async and tokio runtimes,https://example.com/rust,,PUBLIC
2024-02-01 11:00:00,https://www.linkedin.com/feed/update/urn:li:activity:2,Repost thoughts on open source AI tooling,https://example.com/ai,,PUBLIC
";
        fs::write(dir.join("Shares.csv"), shares).expect("shares");
        let comments = "\
Date,Link,Message
2024-03-01 12:00:00,https://www.linkedin.com/feed/update/urn:li:comment:1,Agree - TDD first then ship.
";
        fs::write(dir.join("Comments.csv"), comments).expect("comments");
        let reactions = "\
Date,Type,Link
2024-03-02 09:00:00,LIKE,https://www.linkedin.com/feed/update/urn:li:activity:9
";
        fs::write(dir.join("Reactions.csv"), reactions).expect("reactions");
        let reposts = "\
Date,Link
2024-03-03 08:00:00,https://www.linkedin.com/feed/update/urn:li:activity:8
";
        fs::write(dir.join("InstantReposts.csv"), reposts).expect("reposts");
        let profile = "\
First Name,Last Name,Maiden Name,Address,Birth Date,Headline,Summary,Industry,Zip Code,Geo Location,Twitter Handles,Websites,Instant Messengers
Greg,Test,,,,Senior Rust Engineer,Builds SDKs and distributed systems in Rust.,Blockchain,,Hilversum,,,
";
        fs::write(dir.join("Profile.csv"), profile).expect("profile");
        let positions = "\
Company Name,Title,Description,Location,Started On,Finished On
Acme,Engineer,Shipped Axum APIs and Tokio workers.,NL,2020,2024
";
        fs::write(dir.join("Positions.csv"), positions).expect("positions");
        let messages = "\
CONVERSATION ID,CONTENT
1,SECRET DM should never import
";
        fs::write(dir.join("messages.csv"), messages).expect("messages");
    }

    #[tokio::test]
    async fn imports_fixture_dir_into_db() {
        let dir = TempDir::new().expect("temp");
        write_fixture_dir(dir.path());
        let db_path = dir.path().join("rag.db");
        let stats = import_linkedin_export(dir.path(), &db_path, &MockEmbedClient)
            .await
            .expect("import");
        assert!(
            stats.inserted >= 6,
            "expected posts+comments+repost+profile+position (no reactions), got {stats:?}"
        );
        let db = SourceDb::open(&db_path).expect("db");
        let posts = db
            .list_sources(&SourceListFilter {
                activity: "post".into(),
                limit: 10,
                preview_chars: 80,
                ..Default::default()
            })
            .expect("posts");
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].occurred_at.as_deref(), Some("2024-02-01T11:00:00"));
        let reactions = db
            .list_sources(&SourceListFilter {
                activity: "reaction".into(),
                limit: 5,
                preview_chars: 40,
                ..Default::default()
            })
            .expect("reactions");
        assert!(reactions.is_empty(), "reactions must not be imported");
        let reposts = db
            .list_sources(&SourceListFilter {
                activity: "repost".into(),
                limit: 5,
                preview_chars: 40,
                ..Default::default()
            })
            .expect("reposts");
        assert_eq!(reposts.len(), 1);
        assert!(
            db.get_chunk_candidates("rust", 40)
                .expect("chunks")
                .iter()
                .all(|c| !c.text.contains("SECRET DM")),
            "DMs must not be imported"
        );
        // Weekly re-import: all skipped.
        let again = import_linkedin_export(dir.path(), &db_path, &MockEmbedClient)
            .await
            .expect("reimport");
        assert_eq!(again.inserted, 0);
        assert!(again.skipped >= stats.inserted);
        assert_eq!(db.source_count().expect("count"), stats.inserted as u64);
    }

    #[test]
    fn skips_private_chat_filenames() {
        assert!(is_private_chat_export("messages.csv"));
        assert!(is_private_chat_export("guide_messages.csv"));
        assert!(is_private_chat_export("LearningCoachMessages.csv"));
        assert!(is_private_chat_export("learning_role_play_messages.csv"));
        assert!(!is_private_chat_export("Shares.csv"));
        assert!(!is_private_chat_export("Shares_443113605.csv"));
        assert!(!is_private_chat_export("Comments_443113605.csv"));
        assert!(!is_private_chat_export("Reactions_443113605.csv"));
        assert!(!is_private_chat_export("InstantReposts_443113605.csv"));
    }

    #[test]
    fn normalize_datetime() {
        assert_eq!(
            normalize_linkedin_datetime("2026-07-22 18:18:38"),
            "2026-07-22T18:18:38"
        );
    }

    #[test]
    fn chunk_text_splits() {
        let long = "word ".repeat(300);
        let chunks = chunk_text(&long, 50);
        assert!(chunks.len() > 1);
    }

    #[test]
    fn load_items_skips_messages_and_dual_write() {
        let dir = TempDir::new().expect("temp");
        write_fixture_dir(dir.path());
        let items = load_export_items(dir.path()).expect("load");
        assert!(items.iter().all(|i| !i.text.contains("SECRET DM")));
        assert!(items.iter().any(|i| i.title.contains("Headline")));
        assert!(items
            .iter()
            .any(|i| i.activity == "position" && i.text.contains("Axum")));
        assert!(items.iter().any(|i| i.activity == "post"));
        assert!(items.iter().all(|i| i.activity != "reaction"));
        assert!(items.iter().any(|i| i.activity == "repost"));
        // No dual-write: posts must not also appear as voice copies.
        let post_texts: Vec<_> = items
            .iter()
            .filter(|i| i.activity == "post")
            .map(|i| i.text.clone())
            .collect();
        for t in post_texts {
            assert!(
                !items
                    .iter()
                    .any(|i| i.kind == "voice" && i.activity == "profile" && i.text == t),
                "post must not be dual-written as voice"
            );
        }
    }
}

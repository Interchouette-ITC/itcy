-- Sources corpus: LinkedIn personal activity + URL ingest.
-- kind = contract bucket (voice / personal_feed / comment / url).
-- activity = fine type for queries (post / repost / comment / profile / …).
-- occurred_at = LinkedIn event time (sortable); created_at = row insert time.
-- enrich_* = Tor URL enrich queue (link-only post/repost stubs). Reactions not stored.
-- Indexes that need activity/occurred_at are created in SourceDb::migrate_sources_columns.

CREATE TABLE IF NOT EXISTS sources (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    activity TEXT NOT NULL DEFAULT 'unknown',
    subject TEXT NOT NULL,
    title TEXT NOT NULL,
    url TEXT,
    raw_text TEXT NOT NULL,
    occurred_at TEXT,
    created_at TEXT NOT NULL,
    enrich_status TEXT NOT NULL DEFAULT 'none',
    enrich_after TEXT,
    enrich_claimed_at TEXT,
    enriched_at TEXT
);

CREATE INDEX IF NOT EXISTS sources_kind_subject ON sources (kind, subject);

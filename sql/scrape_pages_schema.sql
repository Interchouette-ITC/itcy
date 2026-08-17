-- Durable Tor scrape results by URL (not LinkedIn sources).
-- Survives sources wipe/rebuild so enrich never re-fetches the same page.

CREATE TABLE IF NOT EXISTS scrape_pages (
    url TEXT PRIMARY KEY NOT NULL,
    fetched_at TEXT NOT NULL,
    http_status INTEGER,
    raw_html TEXT NOT NULL,
    extracted_text TEXT NOT NULL,
    ok INTEGER NOT NULL DEFAULT 0
);

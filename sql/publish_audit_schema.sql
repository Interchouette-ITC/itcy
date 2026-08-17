-- Ship audit trail for company-page publish (mock or live) after BAT merge.

CREATE TABLE IF NOT EXISTS publish_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    draft_id TEXT,
    pubs_pr_number INTEGER,
    mode TEXT NOT NULL,
    status TEXT NOT NULL,
    linkedin_urn TEXT,
    linkedin_url TEXT,
    error TEXT,
    body_preview TEXT NOT NULL DEFAULT '',
    body_sha256 TEXT NOT NULL DEFAULT '',
    detail TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS publish_audit_created
    ON publish_audit (created_at);

CREATE INDEX IF NOT EXISTS publish_audit_draft
    ON publish_audit (draft_id);

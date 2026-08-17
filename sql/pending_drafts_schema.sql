-- Last grounded draft waiting for BAT submit (one row).

CREATE TABLE IF NOT EXISTS pending_drafts (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    subject TEXT NOT NULL,
    body TEXT NOT NULL,
    model TEXT NOT NULL,
    tokens_in INTEGER NOT NULL,
    tokens_out INTEGER NOT NULL,
    sources_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

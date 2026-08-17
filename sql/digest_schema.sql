-- Daily subject digests (choices for /propose_draft). Tweet bodies are not stored in sources.

CREATE TABLE IF NOT EXISTS digests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    digest_id TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL DEFAULT 'open',
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS digest_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    digest_id TEXT NOT NULL,
    idx INTEGER NOT NULL,
    title TEXT NOT NULL,
    url TEXT,
    subject TEXT NOT NULL,
    lane TEXT NOT NULL,
    weight INTEGER NOT NULL DEFAULT 0,
    detail TEXT NOT NULL DEFAULT '',
    UNIQUE (digest_id, idx)
);

CREATE INDEX IF NOT EXISTS digest_items_digest ON digest_items (digest_id);

CREATE TABLE IF NOT EXISTS digest_code_seq (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    next_ord INTEGER NOT NULL
);
INSERT OR IGNORE INTO digest_code_seq (id, next_ord) VALUES (1, 0);

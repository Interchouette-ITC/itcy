-- Operator drafts keyed by Draft ID (rework until accept → BAT → Post).
-- status: building | open | accepted | published | failed

CREATE TABLE IF NOT EXISTS drafts (
    draft_id TEXT PRIMARY KEY NOT NULL,
    subject TEXT NOT NULL,
    body TEXT NOT NULL,
    model TEXT NOT NULL,
    tokens_in INTEGER NOT NULL,
    tokens_out INTEGER NOT NULL,
    sources_json TEXT NOT NULL,
    link_options_json TEXT NOT NULL DEFAULT '[]',
    research_pack TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'open',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    fork_pr_number INTEGER,
    fork_pr_url TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS drafts_status_updated
    ON drafts (status, updated_at);

-- Tweet IDs reuse the drafts table (TWEET- prefix). Separate sequence from LinkedIn drafts.
CREATE TABLE IF NOT EXISTS tweet_code_seq (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    next_ord INTEGER NOT NULL
);
INSERT OR IGNORE INTO tweet_code_seq (id, next_ord) VALUES (1, 0);

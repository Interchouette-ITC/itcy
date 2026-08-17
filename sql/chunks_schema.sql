-- Chunks with embeddings for subject RAG (f32 little-endian BLOB).

CREATE TABLE IF NOT EXISTS chunks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id INTEGER NOT NULL,
    subject TEXT NOT NULL,
    text TEXT NOT NULL,
    embedding BLOB NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (source_id) REFERENCES sources (id)
);

CREATE INDEX IF NOT EXISTS chunks_subject ON chunks (subject);

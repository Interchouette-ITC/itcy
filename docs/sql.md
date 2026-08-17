# SQLite

ITCy has no migration tree. Schema and statements are the `sql/*.sql` files in this repository. The running process opens SQLite and applies those statements.

## Runtime file

`sql/runtime.db` (plus `-wal` / `-shm` while open) is created on first open. It holds sources, chunks, Slack memory, drafts, digest, and publish audit. **Do not commit** `*.db` files.

A separate scrape cache may exist as `sql/linkedin-scrape-cache.db` (also gitignored).

## Schema files

| File | Role |
| --- | --- |
| `sql/sources_schema.sql` | Corpus rows (export, enrich, ingest) |
| `sql/chunks_schema.sql` | RAG chunks |
| `sql/messages_schema.sql` | Slack thread memory |
| `sql/drafts_schema.sql` | LinkedIn drafts and X tweets |
| `sql/pending_drafts_schema.sql` | In-flight draft build |
| `sql/digest_schema.sql` | Daily digest |
| `sql/publish_audit_schema.sql` | Ship audit |
| `sql/scrape_pages_schema.sql` | LinkedIn scrape page cache |

Neighbor `*_insert.sql` / `*_get.sql` / `*_list.sql` files are the statements the Rust store uses. Edit schema and statements together; open the database with the product binary so the file matches the code.

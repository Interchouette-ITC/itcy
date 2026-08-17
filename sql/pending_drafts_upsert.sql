INSERT INTO pending_drafts (
    id, subject, body, model, tokens_in, tokens_out, sources_json, created_at
) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
ON CONFLICT(id) DO UPDATE SET
    subject = excluded.subject,
    body = excluded.body,
    model = excluded.model,
    tokens_in = excluded.tokens_in,
    tokens_out = excluded.tokens_out,
    sources_json = excluded.sources_json,
    created_at = excluded.created_at;

INSERT INTO drafts (
    draft_id, subject, body, model, tokens_in, tokens_out,
    sources_json, link_options_json, research_pack, status, created_at, updated_at,
    fork_pr_number, fork_pr_url
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
ON CONFLICT(draft_id) DO UPDATE SET
    subject = excluded.subject,
    body = excluded.body,
    model = excluded.model,
    tokens_in = excluded.tokens_in,
    tokens_out = excluded.tokens_out,
    sources_json = excluded.sources_json,
    link_options_json = excluded.link_options_json,
    research_pack = excluded.research_pack,
    status = excluded.status,
    updated_at = excluded.updated_at,
    fork_pr_number = COALESCE(excluded.fork_pr_number, drafts.fork_pr_number),
    fork_pr_url = CASE
        WHEN excluded.fork_pr_url != '' THEN excluded.fork_pr_url
        ELSE drafts.fork_pr_url
    END;

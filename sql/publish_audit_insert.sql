INSERT INTO publish_audit (
    draft_id,
    pubs_pr_number,
    mode,
    status,
    linkedin_urn,
    linkedin_url,
    error,
    body_preview,
    body_sha256,
    detail,
    created_at
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11);

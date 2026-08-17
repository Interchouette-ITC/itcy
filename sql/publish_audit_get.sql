SELECT
    id,
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
FROM publish_audit
WHERE id = ?1;

SELECT id,
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
WHERE status = 'ok'
  AND (
        (?1 IS NOT NULL AND draft_id = ?1)
        OR (?2 IS NOT NULL AND pubs_pr_number = ?2)
      )
ORDER BY id DESC
LIMIT 1;

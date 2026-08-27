SELECT subject, substr(body, 1, 400)
FROM drafts
WHERE (draft_id LIKE 'DRAFT-%' OR draft_id LIKE 'TWEET-%')
  AND status IN ('building', 'open', 'accepted', 'published')
ORDER BY updated_at DESC
LIMIT ?1;

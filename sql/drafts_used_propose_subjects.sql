SELECT subject
FROM drafts
WHERE (draft_id LIKE 'DRAFT-%' OR draft_id LIKE 'TWEET-%')
  AND status IN ('open', 'accepted', 'published')
  AND trim(subject) != ''
  AND lower(trim(subject)) NOT IN ('what we know', 'unknown', 'digest item')
ORDER BY updated_at DESC
LIMIT ?1;

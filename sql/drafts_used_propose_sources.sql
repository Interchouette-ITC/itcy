-- Cite / pack URLs already used on open, accepted, or published drafts/tweets.
SELECT sources_json
FROM drafts
WHERE (draft_id LIKE 'DRAFT-%' OR draft_id LIKE 'TWEET-%')
  AND status IN ('open', 'accepted', 'published')
  AND trim(sources_json) != ''
  AND sources_json != '[]'
ORDER BY updated_at DESC
LIMIT ?1;

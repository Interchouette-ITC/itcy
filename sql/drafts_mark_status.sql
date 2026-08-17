UPDATE drafts
SET status = ?2, updated_at = ?3
WHERE draft_id = ?1 AND status = 'open';

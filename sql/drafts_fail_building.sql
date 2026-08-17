UPDATE drafts
SET status = 'failed', updated_at = ?1
WHERE status = 'building';

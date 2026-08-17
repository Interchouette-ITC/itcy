SELECT digest_id, status, created_at FROM digests
WHERE status = 'open'
ORDER BY id DESC
LIMIT 1;

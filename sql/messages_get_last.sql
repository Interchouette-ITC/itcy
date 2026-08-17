SELECT role, content
FROM messages
WHERE session_id = ?1
ORDER BY created_at DESC
LIMIT ?2;

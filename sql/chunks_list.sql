SELECT id, source_id, subject, substr(text, 1, ?3) AS preview, length(text) AS text_len, created_at
FROM chunks
WHERE (?1 = 0 OR source_id = ?1)
  AND (?2 = '' OR lower(subject) LIKE '%' || lower(?2) || '%')
ORDER BY id DESC
LIMIT ?4;

SELECT id, source_id, subject, text, embedding
FROM chunks
WHERE (?1 = '' OR lower(subject) LIKE '%' || lower(?1) || '%')
ORDER BY id DESC
LIMIT ?2;

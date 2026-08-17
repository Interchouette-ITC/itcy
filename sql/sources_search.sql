SELECT id, kind, activity, subject, title, url,
       length(raw_text) AS text_len,
       substr(raw_text, 1, ?3) AS preview,
       occurred_at, created_at
FROM sources
WHERE (?1 = '' OR activity = ?1)
  AND (lower(title) LIKE '%' || lower(?2) || '%'
       OR lower(raw_text) LIKE '%' || lower(?2) || '%')
ORDER BY IFNULL(occurred_at, '') DESC, id DESC
LIMIT ?4;

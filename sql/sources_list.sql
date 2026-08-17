SELECT id, kind, activity, subject, title, url,
       length(raw_text) AS text_len,
       substr(raw_text, 1, ?4) AS preview,
       occurred_at, created_at
FROM sources
WHERE (?1 = '' OR activity = ?1)
  AND (?2 = '' OR lower(subject) LIKE '%' || lower(?2) || '%')
  AND (?3 = '' OR lower(title) LIKE '%' || lower(?3) || '%')
ORDER BY IFNULL(occurred_at, '') DESC, id DESC
LIMIT ?5 OFFSET ?6;

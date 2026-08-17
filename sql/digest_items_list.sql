SELECT idx, title, url, subject, lane, weight, detail
FROM digest_items
WHERE digest_id = ?1
ORDER BY idx ASC;

SELECT 1
FROM sources
WHERE activity = ?1
  AND IFNULL(url, '') = IFNULL(?2, '')
  AND IFNULL(occurred_at, '') = IFNULL(?3, '')
  AND title = ?4
LIMIT 1;

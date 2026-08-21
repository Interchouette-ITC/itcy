-- Prior-calendar-day digest item keys for cross-day freshness.
-- Bounds are DIGEST-YYYYMMDD prefixes: >= lookback and < today (exclusive).
SELECT di.url, di.title
FROM digest_items di
INNER JOIN digests d ON d.digest_id = di.digest_id
WHERE d.digest_id >= ?1 AND d.digest_id < ?2;

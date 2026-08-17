SELECT id, kind, activity, subject, title, url, raw_text, occurred_at,
       enrich_status, enrich_after, enrich_claimed_at, enriched_at
FROM sources
WHERE id = ?1;

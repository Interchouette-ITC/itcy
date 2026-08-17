SELECT kind, activity, COUNT(*) AS n
FROM sources
GROUP BY kind, activity
ORDER BY kind, activity;

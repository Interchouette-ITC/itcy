UPDATE drafts
SET fork_pr_number = ?2, fork_pr_url = ?3, updated_at = ?4
WHERE draft_id = ?1;

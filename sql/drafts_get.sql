SELECT draft_id, subject, body, model, tokens_in, tokens_out,
       sources_json, link_options_json, research_pack, status, created_at, updated_at,
       fork_pr_number, fork_pr_url
FROM drafts
WHERE draft_id = ?1;

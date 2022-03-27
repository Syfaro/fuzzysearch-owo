SELECT
    id,
    owner_id,
    source_site "source_site: Site",
    username,
    last_update,
    loading_state "loading_state: sqlx::types::Json<LoadingState>",
    data
FROM
    linked_account
WHERE
    owner_id = $1
    AND source_site = $2
    AND data ->> 'site_id' = $3;

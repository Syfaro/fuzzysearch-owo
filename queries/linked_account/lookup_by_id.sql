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
    id = $1;

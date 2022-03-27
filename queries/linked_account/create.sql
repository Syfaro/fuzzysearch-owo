INSERT INTO
    linked_account (
        owner_id,
        source_site,
        username,
        data
    )
VALUES
    ($1, $2, $3, $4) RETURNING id,
    owner_id,
    source_site "source_site: Site",
    username,
    last_update,
    loading_state "loading_state: sqlx::types::Json<LoadingState>",
    data;

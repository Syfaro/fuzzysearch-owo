SELECT
    id,
    owner_id,
    source_site,
    username,
    last_update,
    loading_state,
    data
FROM
    linked_account
WHERE
    id = $1;

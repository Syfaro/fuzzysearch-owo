INSERT INTO
    linked_account (
        owner_id,
        source_site,
        username,
        data
    )
VALUES
    ($1, $2, $3, $4) RETURNING *;

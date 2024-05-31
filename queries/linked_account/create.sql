INSERT INTO
    linked_account (
        owner_id,
        source_site,
        username,
        data,
        verification_key,
        verified_at
    )
VALUES
    ($1, $2, $3, $4, $5, $6) RETURNING *;

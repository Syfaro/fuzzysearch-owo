INSERT INTO
    owned_media_item (
        owner_id,
        account_id,
        source_id,
        perceptual_hash,
        sha256_hash,
        link,
        title,
        posted_at,
        last_modified
    )
VALUES
    ($1, $2, $3, $4, $5, $6, $7, $8, current_timestamp) RETURNING id;

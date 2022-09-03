INSERT INTO
    owned_media_item (
        owner_id,
        perceptual_hash,
        sha256_hash,
        posted_at,
        last_modified,
        title
    )
VALUES
    (
        $1,
        $2,
        $3,
        current_timestamp,
        current_timestamp,
        $4
    ) RETURNING id;

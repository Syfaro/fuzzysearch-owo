SELECT
    *
FROM
    owned_media_item
WHERE
    perceptual_hash <@ ($1, $2);

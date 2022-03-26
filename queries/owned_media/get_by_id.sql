SELECT
    id,
    owner_id,
    account_id,
    source_id,
    perceptual_hash,
    sha256_hash "sha256_hash: Sha256Hash",
    link,
    title,
    posted_at,
    last_modified,
    content_url,
    content_size,
    thumb_url
FROM
    owned_media_item
WHERE
    id = $1;

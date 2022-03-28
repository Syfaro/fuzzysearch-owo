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
    thumb_url,
    event_count,
    last_event
FROM
    owned_media_item
WHERE
    owner_id = $1
ORDER BY
    last_event DESC NULLS LAST,
    last_modified DESC
LIMIT
    $2 OFFSET ($3::integer * $2::integer);

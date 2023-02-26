SELECT
    id,
    content_url,
    thumb_url
FROM
    owned_media_item
WHERE
    content_url NOT LIKE $1
    AND content_url IS NOT NULL
LIMIT
    $2;

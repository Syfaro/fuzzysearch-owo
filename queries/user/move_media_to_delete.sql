INSERT INTO
    pending_deletion (url)
SELECT
    content_url
FROM
    owned_media_item
WHERE
    owner_id = $1
    AND content_url IS NOT NULL
UNION
SELECT
    thumb_url
FROM
    owned_media_item
WHERE
    owner_id = $1
    AND thumb_url IS NOT NULL ON CONFLICT DO NOTHING;

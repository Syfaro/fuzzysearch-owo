UPDATE
    owned_media_item
SET
    content_url = $2,
    content_size = $3,
    thumb_url = $4
WHERE
    id = $1;

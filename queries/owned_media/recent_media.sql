SELECT
    *
FROM
    owned_media_item
WHERE
    owner_id = $1
ORDER BY
    last_modified DESC
LIMIT
    30;

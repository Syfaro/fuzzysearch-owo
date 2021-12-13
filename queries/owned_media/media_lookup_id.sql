SELECT
    *
FROM
    owned_media_item
WHERE
    id = $1
    AND owner_id = $2;

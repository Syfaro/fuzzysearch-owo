SELECT
    count(*)
FROM
    owned_media_item
WHERE
    owner_id = $1;

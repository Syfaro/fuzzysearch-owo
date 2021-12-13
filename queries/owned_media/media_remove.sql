DELETE FROM
    owned_media_item
WHERE
    owner_id = $1
    AND id = $2;

UPDATE
    owned_media_item_account
SET
    owned_media_item_id = $2
WHERE
    owned_media_item_id = $1;

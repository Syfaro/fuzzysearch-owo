SELECT
    *
FROM
    user_event
WHERE
    owner_id = $1
    AND related_to_media_item_id = $2
ORDER BY
    created_at DESC
LIMIT
    50;

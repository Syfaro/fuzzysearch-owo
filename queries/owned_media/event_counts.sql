SELECT
    owned_media_item.id,
    count(*) "count!"
FROM
    owned_media_item
    JOIN user_event ON user_event.related_to_media_item_id = owned_media_item.id
WHERE
    owned_media_item.id = any($1)
GROUP BY
    owned_media_item.id

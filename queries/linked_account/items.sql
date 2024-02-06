SELECT
    coalesce(count(*), 0) "count!",
    coalesce(sum(content_size)::bigint, 0) "total_content_size!"
FROM
    owned_media_item
    JOIN owned_media_item_account ON owned_media_item.id = owned_media_item_account.owned_media_item_id
WHERE
    owner_id = $1
    AND owned_media_item_account.account_id = $2;

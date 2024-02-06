INSERT INTO
    pending_deletion (url)
SELECT
    content_url
FROM
    owned_media_item
    JOIN owned_media_item_account ON owned_media_item.id = owned_media_item_account.owned_media_item_id
WHERE
    owned_media_item_account.account_id = $1
    AND content_url IS NOT NULL
UNION
SELECT
    thumb_url
FROM
    owned_media_item
    JOIN owned_media_item_account ON owned_media_item.id = owned_media_item_account.owned_media_item_id
WHERE
    owned_media_item_account.account_id = $1
    AND thumb_url IS NOT NULL ON CONFLICT DO NOTHING;

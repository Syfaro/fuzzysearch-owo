INSERT INTO
    owned_media_item_account (
        owned_media_item_id,
        account_id,
        source_id,
        link,
        title,
        posted_at
    )
SELECT
    id,
    account_id,
    source_id,
    link,
    title,
    posted_at
FROM
    owned_media_item
WHERE
    account_id IS NOT NULL;

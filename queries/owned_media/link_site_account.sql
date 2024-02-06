INSERT INTO
    owned_media_item_account (
        owned_media_item_id,
        account_id,
        source_id,
        link,
        title,
        posted_at
    )
VALUES
    ($1, $2, $3, $4, $5, $6) ON CONFLICT (owned_media_item_id, account_id, source_id) DO
UPDATE
SET
    link = EXCLUDED.link,
    title = EXCLUDED.title,
    posted_at = EXCLUDED.posted_at;

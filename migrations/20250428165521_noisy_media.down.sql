CREATE OR REPLACE VIEW owned_media_item_accounts (
    id,
    owner_id,
    perceptual_hash,
    sha256_hash,
    last_modified,
    content_url,
    content_size,
    thumb_url,
    event_count,
    last_event,
    accounts,
    noisy_media
) AS
SELECT
    owned_media_item.id,
    owned_media_item.owner_id,
    owned_media_item.perceptual_hash,
    owned_media_item.sha256_hash,
    owned_media_item.last_modified,
    owned_media_item.content_url,
    owned_media_item.content_size,
    owned_media_item.thumb_url,
    owned_media_item.event_count,
    owned_media_item.last_event,
    jsonb_agg(
        jsonb_build_object(
            'account_id',
            owned_media_item_account.account_id,
            'source_id',
            owned_media_item_account.source_id,
            'link',
            owned_media_item_account.link,
            'title',
            owned_media_item_account.title,
            'posted_at',
            owned_media_item_account.posted_at
        )
    ) FILTER (
        WHERE
            owned_media_item_account.owned_media_item_id IS NOT NULL
    ) accounts,
    NULL::boolean
FROM
    owned_media_item
    LEFT JOIN owned_media_item_account ON owned_media_item.id = owned_media_item_account.owned_media_item_id
GROUP BY
    owned_media_item.id;

DROP TRIGGER update_owned_media_item_noisy_trigger ON owned_media_item;
DROP FUNCTION update_owned_media_item_noisy;

ALTER TABLE
    owned_media_item DROP COLUMN noisy_media;

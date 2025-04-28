ALTER TABLE
    owned_media_item
ADD
    COLUMN noisy_media BOOLEAN;

CREATE FUNCTION update_owned_media_item_noisy() RETURNS TRIGGER AS $$
BEGIN
    IF NEW.event_count > 64 AND NEW.noisy_media IS NULL THEN
        UPDATE
            owned_media_item
        SET
            noisy_media = true
        WHERE
            id = NEW.id;
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER update_owned_media_item_noisy_trigger
AFTER
UPDATE
    OF event_count ON owned_media_item FOR EACH ROW EXECUTE PROCEDURE update_owned_media_item_noisy();

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
    owned_media_item.noisy_media
FROM
    owned_media_item
    LEFT JOIN owned_media_item_account ON owned_media_item.id = owned_media_item_account.owned_media_item_id
GROUP BY
    owned_media_item.id;

UPDATE
    owned_media_item
SET
    noisy_media = TRUE
WHERE
    event_count > 64;

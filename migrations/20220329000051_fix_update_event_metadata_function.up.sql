CREATE OR REPLACE FUNCTION update_media_event_metadata() RETURNS TRIGGER AS $$
BEGIN
    UPDATE
        owned_media_item
    SET
        event_count = (
            SELECT
                count(*)
            FROM
                user_event
            WHERE
                user_event.related_to_media_item_id = NEW.related_to_media_item_id
        ),
        last_event = (
            SELECT
                max(created_at)
            FROM
                user_event
            WHERE
                user_event.related_to_media_item_id = NEW.related_to_media_item_id
        )
    WHERE
        owned_media_item.id = NEW.related_to_media_item_id;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

UPDATE
    owned_media_item
SET
    event_count = (
        SELECT
            count(*)
        FROM
            user_event
        WHERE
            user_event.related_to_media_item_id = owned_media_item.id
    ),
    last_event = (
        SELECT
            max(created_at)
        FROM
            user_event
        WHERE
            user_event.related_to_media_item_id = owned_media_item.id
    );

ALTER TABLE
    owned_media_item
ADD
    COLUMN event_count integer NOT NULL DEFAULT 0,
ADD
    COLUMN last_event timestamp with time zone;

CREATE FUNCTION update_media_event_metadata() RETURNS TRIGGER AS $$
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
                user_event.related_to_media_item_id = NEW.id
        ),
        last_event = (
            SELECT
                max(created_at)
            FROM
                user_event
            WHERE
                user_event.related_to_media_item_id = NEW.id
        )
    WHERE
        owned_media_item.id = NEW.id;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER update_media_event_metadata_trigger
AFTER
INSERT
    ON user_event FOR EACH ROW EXECUTE PROCEDURE update_media_event_metadata();

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

CREATE INDEX owned_media_item_owner_id_idx ON owned_media_item (owner_id);

ALTER TABLE
    owned_media_item DROP COLUMN event_count,
    DROP COLUMN last_event;

DROP TRIGGER update_media_event_metadata_trigger ON user_event;
DROP FUNCTION update_media_event_metadata;

DROP INDEX owned_media_item_owner_id_idx;

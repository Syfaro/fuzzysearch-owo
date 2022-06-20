ALTER TABLE
    user_event
ADD
    COLUMN last_updated timestamp with time zone;

UPDATE
    user_event
SET
    last_updated = created_at;

ALTER TABLE
    user_event
ALTER COLUMN
    last_updated
SET
    NOT NULL,
ALTER COLUMN
    last_updated
SET
    DEFAULT current_timestamp;

CREATE INDEX user_event_filter_idx ON user_event (owner_id, event_name, last_updated DESC);

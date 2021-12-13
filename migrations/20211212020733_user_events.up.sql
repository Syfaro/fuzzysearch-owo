CREATE TABLE user_event (
    id uuid PRIMARY KEY default gen_random_uuid(),
    owner_id uuid NOT NULL REFERENCES user_account (id) ON DELETE CASCADE,
    related_to_media_item_id uuid REFERENCES owned_media_item (id) ON DELETE CASCADE,
    created_at timestamp with time zone NOT NULL DEFAULT current_timestamp,
    message text NOT NULL,
    event_name text,
    data jsonb
);

CREATE INDEX user_event_created_at ON user_event (owner_id, created_at);

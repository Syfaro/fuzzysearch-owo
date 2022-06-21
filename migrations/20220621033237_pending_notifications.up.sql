CREATE TABLE pending_notification (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id uuid NOT NULL REFERENCES user_account (id) ON DELETE CASCADE,
    user_event_id uuid NOT NULL REFERENCES user_event (id) ON DELETE CASCADE,
    created_at timestamp with time zone NOT NULL DEFAULT current_timestamp
);

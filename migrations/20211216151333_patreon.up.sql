CREATE TABLE patreon_webhook_event (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    linked_account_id uuid REFERENCES linked_account (id) ON DELETE CASCADE,
    received_at timestamp with time zone NOT NULL DEFAULT current_timestamp,
    data jsonb NOT NULL
);

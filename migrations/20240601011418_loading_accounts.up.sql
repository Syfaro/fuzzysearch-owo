CREATE TABLE linked_account_import (
    linked_account_id uuid PRIMARY KEY REFERENCES linked_account (id) ON DELETE CASCADE,
    started_at timestamp with time zone NOT NULL DEFAULT current_timestamp,
    completed_at timestamp with time zone,
    expected_count integer NOT NULL,
    expected_ids text[] NOT NULL,
    loaded_ids text[] NOT NULL DEFAULT array[]::text[]
);

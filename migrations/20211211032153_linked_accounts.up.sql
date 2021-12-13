CREATE TABLE linked_account (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id uuid NOT NULL REFERENCES user_account (id) ON DELETE CASCADE,
    source_site text NOT NULL,
    username text NOT NULL,
    last_update timestamp with time zone,
    loading_state JSONB,
    credentials JSONB
);

CREATE UNIQUE INDEX linked_account_unique ON linked_account (source_site, lower(username));

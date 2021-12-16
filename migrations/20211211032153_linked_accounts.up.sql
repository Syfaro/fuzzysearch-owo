CREATE TABLE linked_account (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id uuid NOT NULL REFERENCES user_account (id) ON DELETE CASCADE,
    source_site text NOT NULL,
    username text NOT NULL,
    last_update timestamp with time zone,
    loading_state jsonb,
    data jsonb
);

CREATE UNIQUE INDEX linked_account_unique_idx ON linked_account (source_site, lower(username));

CREATE INDEX linked_account_site_id_idx ON linked_account ((data ->> 'site_id'));

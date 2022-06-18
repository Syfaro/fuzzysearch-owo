CREATE TABLE user_allowlist (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id uuid NOT NULL REFERENCES user_account (id) ON DELETE CASCADE,
    site text NOT NULL,
    site_username text NOT NULL
);

CREATE UNIQUE INDEX post_allowlist_site_username_idx ON user_allowlist (owner_id, site, lower(site_username));

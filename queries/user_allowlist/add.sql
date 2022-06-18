INSERT INTO
    user_allowlist (owner_id, site, site_username)
VALUES
    ($1, $2, $3) ON CONFLICT DO NOTHING RETURNING id;

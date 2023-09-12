INSERT INTO
    bluesky_post (repo, cid, created_at)
VALUES
    ($1, $2, $3) ON CONFLICT DO NOTHING;

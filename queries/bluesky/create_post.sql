INSERT INTO
    bluesky_post (repo, rkey, created_at)
VALUES
    ($1, $2, $3) ON CONFLICT DO NOTHING RETURNING repo;

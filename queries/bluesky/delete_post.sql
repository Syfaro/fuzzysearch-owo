INSERT INTO
    bluesky_post (repo, rkey, deleted_at)
VALUES
    ($1, $2, current_timestamp) ON CONFLICT (repo, rkey) DO
UPDATE
SET
    deleted_at = current_timestamp

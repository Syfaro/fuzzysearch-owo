INSERT INTO
    bluesky_post (repo_id, rkey, deleted_at)
VALUES
    ($1, $2, current_timestamp) ON CONFLICT (repo_id, rkey) DO
UPDATE
SET
    deleted_at = current_timestamp

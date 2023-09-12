INSERT INTO
    bluesky_post (repo, cid, deleted_at)
VALUES
    ($1, $2, current_timestamp) ON CONFLICT (repo, cid) DO
UPDATE
SET
    deleted_at = current_timestamp

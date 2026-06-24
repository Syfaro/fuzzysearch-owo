ALTER TABLE bluesky_image DROP CONSTRAINT bluesky_image_post_fkey;

CREATE TABLE bluesky_post_old (
    repo text NOT NULL,
    rkey text NOT NULL,
    created_at timestamptz,
    deleted_at timestamptz,
    PRIMARY KEY (repo, rkey)
);

INSERT INTO bluesky_post_old (repo, rkey, created_at, deleted_at)
    SELECT
        r.did, p.rkey, p.created_at, p.deleted_at
    FROM bluesky_post p
        JOIN bluesky_repo r ON r.id = p.repo_id;

DROP TABLE bluesky_post;

ALTER TABLE bluesky_post_old RENAME TO bluesky_post;

ALTER INDEX bluesky_post_old_pkey RENAME TO bluesky_post_pkey;

CREATE INDEX bluesky_post_created_at_idx ON bluesky_post (created_at) WHERE created_at IS NOT NULL;

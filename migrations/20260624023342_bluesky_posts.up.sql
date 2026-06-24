CREATE TABLE bluesky_post_new (
    repo_id integer NOT NULL,
    rkey text NOT NULL,
    created_at timestamptz,
    deleted_at timestamptz,
    PRIMARY KEY (repo_id, rkey),
    CONSTRAINT bluesky_post_repo_fkey FOREIGN KEY (repo_id) REFERENCES bluesky_repo (id)
);

INSERT INTO bluesky_post_new (repo_id, rkey, created_at, deleted_at)
    SELECT
        r.id, p.rkey, p.created_at, p.deleted_at
    FROM
        bluesky_post p
        JOIN bluesky_repo r ON r.did = p.repo
    WHERE
        p.deleted_at >= current_timestamp - interval '30 days'
        OR EXISTS (SELECT 1 FROM bluesky_image i WHERE i.repo_id = r.id AND i.post_rkey = p.rkey);

DROP TABLE bluesky_post;

ALTER TABLE bluesky_post_new RENAME TO bluesky_post;

ALTER INDEX bluesky_post_new_pkey RENAME TO bluesky_post_pkey;

ALTER TABLE bluesky_image ADD CONSTRAINT bluesky_image_post_fkey FOREIGN KEY (repo_id, post_rkey) REFERENCES bluesky_post (repo_id, rkey) ON DELETE CASCADE;

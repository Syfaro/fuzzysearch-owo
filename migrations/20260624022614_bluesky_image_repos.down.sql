CREATE TABLE bluesky_image_old (
    repo text NOT NULL,
    post_rkey text NOT NULL,
    blob_cid text NOT NULL,
    size bigint NOT NULL,
    sha256 bytea NOT NULL,
    perceptual_hash bigint,
    PRIMARY KEY (repo, post_rkey, blob_cid),
    FOREIGN KEY (repo, post_rkey) REFERENCES bluesky_post (repo, rkey) ON DELETE CASCADE
);

INSERT INTO bluesky_image_old (repo, post_rkey, blob_cid, size, sha256, perceptual_hash)
    SELECT
        r.did, i.post_rkey, i.blob_cid, i.size, i.sha256, i.perceptual_hash
    FROM
        bluesky_image i
        JOIN bluesky_repo r ON r.id = i.repo_id;

DROP TABLE bluesky_image;

ALTER TABLE bluesky_image_old RENAME TO bluesky_image;

ALTER INDEX bluesky_image_old_pkey RENAME TO bluesky_image_pkey;

CREATE INDEX bluesky_image_perceptual_hash_idx ON bluesky_image USING spgist (perceptual_hash bktree_ops);

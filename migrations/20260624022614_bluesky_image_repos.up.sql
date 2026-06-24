CREATE TABLE bluesky_image_new (
    repo_id integer NOT NULL,
    post_rkey text NOT NULL,
    blob_cid text NOT NULL,
    size bigint NOT NULL,
    sha256 bytea NOT NULL,
    perceptual_hash bigint,
    PRIMARY KEY (repo_id, post_rkey, blob_cid)
);

INSERT INTO bluesky_image_new (repo_id, post_rkey, blob_cid, size, sha256, perceptual_hash)
    SELECT
        r.id, i.post_rkey, i.blob_cid, i.size, i.sha256, i.perceptual_hash
    FROM
        bluesky_image i
        JOIN bluesky_repo r ON r.did = i.repo;

DROP TABLE bluesky_image;

ALTER TABLE bluesky_image_new RENAME TO bluesky_image;

ALTER INDEX bluesky_image_new_pkey RENAME TO bluesky_image_pkey;

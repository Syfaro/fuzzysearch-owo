CREATE TABLE bluesky_post (
    repo text NOT NULL,
    rkey text NOT NULL,
    created_at timestamp with time zone,
    deleted_at timestamp with time zone,
    PRIMARY KEY (repo, rkey)
);

CREATE INDEX bluesky_post_created_at_idx ON bluesky_post (created_at) WHERE created_at IS NOT NULL;

CREATE TABLE bluesky_image (
    repo text NOT NULL,
    post_rkey text NOT NULL,
    blob_cid text NOT NULL,
    size bigint NOT NULL,
    sha256 bytea NOT NULL,
    perceptual_hash bigint,
    PRIMARY KEY (repo, post_rkey, blob_cid),
    FOREIGN KEY (repo, post_rkey) REFERENCES bluesky_post (repo, rkey) ON DELETE CASCADE
);

CREATE INDEX bluesky_image_perceptual_hash_idx ON bluesky_image USING spgist (perceptual_hash bktree_ops);

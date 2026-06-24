CREATE INDEX bluesky_image_perceptual_hash_idx ON bluesky_image USING spgist (perceptual_hash bktree_ops);

INSERT INTO
    bluesky_image (
        repo,
        post_rkey,
        blob_cid,
        size,
        sha256,
        perceptual_hash
    )
VALUES
    ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING;

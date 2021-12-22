INSERT INTO
    reddit_image (post_fullname, size, sha256, perceptual_hash)
VALUES
    ($1, $2, $3, $4) ON CONFLICT DO NOTHING RETURNING id;

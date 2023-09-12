SELECT
    bluesky_post.created_at,
    bluesky_image.*
FROM
    bluesky_image
    JOIN bluesky_post ON bluesky_post.repo = bluesky_image.repo
    AND bluesky_post.rkey = bluesky_image.post_rkey
WHERE
    perceptual_hash <@ ($1, $2)
    AND deleted_at IS NULL;

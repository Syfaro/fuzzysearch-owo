SELECT
    bluesky_post.created_at,
    bluesky_repo.did AS "repo!",
    bluesky_image.post_rkey,
    bluesky_image.blob_cid,
    bluesky_image.size,
    bluesky_image.sha256,
    bluesky_image.perceptual_hash
FROM
    bluesky_image
    JOIN bluesky_repo ON bluesky_repo.id = bluesky_image.repo_id
    JOIN bluesky_post ON bluesky_post.repo_id = bluesky_image.repo_id
    AND bluesky_post.rkey = bluesky_image.post_rkey
WHERE
    perceptual_hash <@ ($1, $2)
    AND deleted_at IS NULL;

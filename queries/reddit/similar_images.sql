SELECT
    fullname "fullname!",
    subreddit_name "subreddit_name!",
    posted_at "posted_at!",
    author "author!",
    permalink "permalink!",
    content_link "content_link!",
    id "id!",
    post_fullname "post_fullname!",
    size "size!",
    sha256 "sha256!",
    perceptual_hash
FROM
    reddit_image
    LEFT JOIN reddit_post ON reddit_image.post_fullname = reddit_post.fullname
WHERE
    perceptual_hash <@ ($1, $2)

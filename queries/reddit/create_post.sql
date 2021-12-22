INSERT INTO
    reddit_post (
        fullname,
        subreddit_name,
        posted_at,
        author,
        permalink,
        content_link
    )
VALUES
    ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING;

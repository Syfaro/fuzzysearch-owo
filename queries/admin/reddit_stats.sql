SELECT
    COALESCE(
        (
            SELECT
                count(*)
            FROM
                reddit_post
            WHERE
                posted_at > current_timestamp - interval '24 hours'
        ),
        0
    ) "recent_posts!";

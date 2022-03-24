SELECT
    *
FROM
    reddit_subreddit
ORDER BY
    disabled ASC,
    last_updated ASC NULLS FIRST;

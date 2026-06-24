SELECT
    name,
    last_updated,
    last_page,
    disabled
FROM
    reddit_subreddit
ORDER BY
    disabled ASC,
    last_updated ASC NULLS FIRST;

SELECT
    name,
    last_updated,
    last_page,
    disabled
FROM
    reddit_subreddit
WHERE
    name = $1;

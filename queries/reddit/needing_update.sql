SELECT
    *
FROM
    reddit_subreddit
WHERE
    last_updated IS NULL
    OR last_updated < now() + interval '15 minutes';

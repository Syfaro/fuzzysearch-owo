UPDATE
    reddit_subreddit
SET
    disabled = $2
WHERE
    name = $1;

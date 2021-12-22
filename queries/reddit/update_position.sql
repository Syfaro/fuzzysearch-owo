UPDATE
    reddit_subreddit
SET
    last_updated = current_timestamp,
    last_page = $2
WHERE
    name = $1;

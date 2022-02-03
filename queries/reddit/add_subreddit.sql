INSERT INTO
    reddit_subreddit (name)
VALUES
    (lower($1)) ON CONFLICT DO NOTHING;

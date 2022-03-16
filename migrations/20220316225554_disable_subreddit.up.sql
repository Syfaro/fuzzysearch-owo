ALTER TABLE
    reddit_subreddit
ADD
    COLUMN disabled boolean DEFAULT FALSE NOT NULL;

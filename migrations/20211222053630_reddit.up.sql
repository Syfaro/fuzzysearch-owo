CREATE TABLE reddit_subreddit (
    name text PRIMARY KEY,
    last_updated timestamp with time zone,
    last_page text
);

CREATE TABLE reddit_post (
    fullname text PRIMARY KEY UNIQUE,
    subreddit_name text NOT NULL REFERENCES reddit_subreddit (name) ON DELETE CASCADE,
    posted_at timestamp with time zone NOT NULL,
    author text NOT NULL,
    permalink text NOT NULL,
    content_link text NOT NULL
);

CREATE TABLE reddit_image (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    post_fullname text NOT NULL REFERENCES reddit_post (fullname) ON DELETE CASCADE,
    size int NOT NULL,
    sha256 bytea NOT NULL,
    perceptual_hash bigint,
    UNIQUE (post_fullname, sha256)
);

CREATE INDEX reddit_image_perceptual_hash_idx ON reddit_image USING spgist (perceptual_hash bktree_ops);

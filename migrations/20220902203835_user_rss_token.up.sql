ALTER TABLE
    user_account
ADD
    COLUMN rss_token uuid UNIQUE NOT NULL DEFAULT gen_random_uuid();

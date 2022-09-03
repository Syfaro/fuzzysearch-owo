ALTER TABLE
    user_account
ADD
    COLUMN api_token uuid UNIQUE NOT NULL DEFAULT gen_random_uuid();

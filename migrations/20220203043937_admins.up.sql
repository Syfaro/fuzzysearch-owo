ALTER TABLE
    user_account
ADD
    COLUMN is_admin bool NOT NULL DEFAULT false;

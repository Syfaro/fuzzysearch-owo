ALTER TABLE
    user_account
ADD
    COLUMN reset_token text UNIQUE;

CREATE UNIQUE INDEX ON user_account (id, reset_token);

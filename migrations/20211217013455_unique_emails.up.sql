DROP INDEX user_account_email_idx;

CREATE UNIQUE INDEX user_account_unique_email_idx ON user_account (lower(email))
WHERE
    email IS NOT NULL;

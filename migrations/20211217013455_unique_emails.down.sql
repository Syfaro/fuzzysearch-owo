DROP INDEX user_account_unique_email_idx;

CREATE INDEX user_account_email_idx ON user_account (lower(email));

ALTER TABLE
    user_account
ADD
    COLUMN email text,
ADD
    COLUMN email_verifier uuid UNIQUE DEFAULT gen_random_uuid();

CREATE INDEX user_account_email_idx ON user_account (lower(email));

ALTER TABLE
    user_account
ADD
    COLUMN unsubscribe_token uuid NOT NULL DEFAULT gen_random_uuid();

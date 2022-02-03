ALTER TABLE
    user_account
ALTER COLUMN
    username DROP NOT NULL,
ALTER COLUMN
    hashed_password DROP NOT NULL,
ADD
    COLUMN telegram_id bigint UNIQUE,
ADD
    COLUMN telegram_name text,
ADD
    CONSTRAINT account_actionable CHECK (
        (
            username IS NOT NULL
            AND hashed_password IS NOT NULL
        )
        OR telegram_id IS NOT NULL
    );

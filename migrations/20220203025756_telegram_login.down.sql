ALTER TABLE
    user_account
ALTER COLUMN
    username
SET
    NOT NULL,
ALTER COLUMN
    hashed_password
SET
    NOT NULL,
    DROP COLUMN telegram_id,
    DROP COLUMN telegram_name;

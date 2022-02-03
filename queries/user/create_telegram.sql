INSERT INTO
    user_account (telegram_id, telegram_name)
VALUES
    ($1, $2) RETURNING id;

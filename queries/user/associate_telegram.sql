UPDATE
    user_account
SET
    telegram_id = $2,
    telegram_name = $3
WHERE
    id = $1;

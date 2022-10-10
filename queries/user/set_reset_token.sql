UPDATE
    user_account
SET
    reset_token = $2
WHERE
    id = $1;

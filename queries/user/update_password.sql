UPDATE
    user_account
SET
    hashed_password = $2,
    reset_token = NULL
WHERE
    id = $1;

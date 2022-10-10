UPDATE
    user_account
SET
    hashed_password = $2
WHERE
    id = $1;

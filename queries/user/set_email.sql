UPDATE
    user_account
SET
    email = $2
WHERE
    id = $1;

SELECT
    id,
    username,
    hashed_password
FROM
    user_account
WHERE
    id = $1;

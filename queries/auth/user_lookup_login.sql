SELECT
    id,
    username,
    hashed_password
FROM
    user_account
WHERE
    lower($1) = lower(username);

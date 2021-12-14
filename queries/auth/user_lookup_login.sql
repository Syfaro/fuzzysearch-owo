SELECT
    *
FROM
    user_account
WHERE
    lower($1) = lower(username);

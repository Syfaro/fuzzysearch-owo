SELECT
    *
FROM
    user_account
WHERE
    lower($1) = lower(username)
    OR (
        email IS NOT NULL
        AND lower($1) = lower(email)
    );

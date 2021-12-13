SELECT
    exists(
        SELECT
            1
        FROM
            user_account
        WHERE
            lower(username) = lower($1)
    ) "exists!";

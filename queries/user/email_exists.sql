SELECT
    exists(
        SELECT
            1
        FROM
            user_account
        WHERE
            lower(email) = lower($1)
    ) "exists!";

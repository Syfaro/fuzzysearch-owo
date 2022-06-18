SELECT
    exists(
        SELECT
            1
        FROM
            user_account
        WHERE
            id = $1
            AND email_verifier = $2
    );

SELECT
    exists(
        SELECT
            1
        FROM
            user_allowlist
        WHERE
            owner_id = $1
            AND site = $2
            AND lower(site_username) = lower($3)
    );

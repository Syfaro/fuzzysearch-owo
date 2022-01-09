SELECT
    *
FROM
    user_session
WHERE
    user_id = $1
ORDER BY
    last_used DESC;

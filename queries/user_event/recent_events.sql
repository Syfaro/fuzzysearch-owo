SELECT
    *
FROM
    user_event
WHERE
    owner_id = $1
ORDER BY
    created_at DESC
LIMIT
    50;

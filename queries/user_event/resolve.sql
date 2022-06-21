SELECT
    *
FROM
    user_event
WHERE
    id = any($1);

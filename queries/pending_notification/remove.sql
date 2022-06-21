DELETE FROM
    pending_notification
WHERE
    id = any($1);

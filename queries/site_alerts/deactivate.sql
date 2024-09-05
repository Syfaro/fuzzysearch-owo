UPDATE
    site_alert
SET
    active = false
WHERE
    id = $1;

SELECT
    id,
    created_at,
    active,
    content
FROM
    site_alert
ORDER BY
    created_at DESC
LIMIT
    50;

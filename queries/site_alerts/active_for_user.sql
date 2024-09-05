SELECT
    id,
    created_at,
    active,
    content
FROM
    site_alert
    LEFT JOIN site_alert_dismiss ON site_alert_dismiss.site_alert_id = site_alert.id AND site_alert_dismiss.user_account_id = $1
WHERE
    site_alert_dismiss.site_alert_id IS NULL
    AND active = true
ORDER BY
    created_at DESC;

INSERT INTO
    site_alert_dismiss (site_alert_id, user_account_id)
VALUES
    ($1, $2) ON CONFLICT DO NOTHING;

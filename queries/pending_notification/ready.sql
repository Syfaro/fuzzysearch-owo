SELECT
    pending_notification.id,
    pending_notification.owner_id,
    pending_notification.created_at,
    pending_notification.user_event_id
FROM
    pending_notification
    JOIN user_setting ON (
        user_setting.owner_id = pending_notification.owner_id
        AND user_setting.setting = 'email-frequency'
    )
WHERE
    user_setting.value = $1;

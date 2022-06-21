INSERT INTO
    pending_notification (owner_id, user_event_id)
VALUES
    ($1, $2) RETURNING id;

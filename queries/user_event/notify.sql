INSERT INTO
    user_event (owner_id, message, event_name)
VALUES
    ($1, $2, $3) RETURNING id;

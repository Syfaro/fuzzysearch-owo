INSERT INTO
    user_session (user_id, source)
VALUES
    ($1, $2) RETURNING id;

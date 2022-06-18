INSERT INTO
    user_session (user_id, source, creation_ip)
VALUES
    ($1, $2, $3) RETURNING id;

INSERT INTO
    auth_state (owner_id, state)
VALUES
    ($1, $2) RETURNING id;

UPDATE
    user_session
SET
    last_used = current_timestamp
WHERE
    user_id = $1
    AND id = $2 RETURNING user_id;

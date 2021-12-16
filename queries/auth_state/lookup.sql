SELECT
    1
FROM
    auth_state
WHERE
    owner_id = $1
    AND state = $2
    AND current_timestamp < expires_after;

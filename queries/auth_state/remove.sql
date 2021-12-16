DELETE FROM
    auth_state
WHERE
    owner_id = $1
    AND state = $2;

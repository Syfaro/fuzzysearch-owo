DELETE FROM
    user_session
WHERE
    user_id = $1
    AND id = $2;

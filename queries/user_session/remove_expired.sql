DELETE FROM
    user_session
WHERE
    last_used < current_timestamp - interval '30 days';

DELETE FROM
    user_account
WHERE
    id = $1 RETURNING id;

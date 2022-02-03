UPDATE
    user_account
SET
    display_name = $2
WHERE
    id = $1;

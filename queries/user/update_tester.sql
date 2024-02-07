UPDATE
    user_account
SET
    is_tester = $2
WHERE
    id = $1;

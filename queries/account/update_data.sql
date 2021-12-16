UPDATE
    linked_account
SET
    data = $2
WHERE
    id = $1;

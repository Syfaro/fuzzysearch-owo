UPDATE
    linked_account
SET
    data = $3
WHERE
    id = $2
    AND owner_id = $1;

SELECT
    *
FROM
    linked_account
WHERE
    id = $1
    AND owner_id = $2;

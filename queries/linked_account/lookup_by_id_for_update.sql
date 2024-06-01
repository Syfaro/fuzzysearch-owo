SELECT
    *
FROM
    linked_account
WHERE
    id = $1
FOR UPDATE;

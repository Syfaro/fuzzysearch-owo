DELETE FROM
    linked_account
WHERE
    owner_id = $1
    AND id = $2;

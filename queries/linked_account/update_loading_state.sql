UPDATE
    linked_account
SET
    loading_state = $3
WHERE
    owner_id = $1
    AND id = $2;

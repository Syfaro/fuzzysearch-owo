UPDATE
    linked_account
SET
    verification_key = NULL,
    verified_at = current_timestamp
WHERE
    id = $1;

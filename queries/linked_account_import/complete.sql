UPDATE
    linked_account_import
SET
    completed_at = current_timestamp
WHERE
    linked_account_id = $1;

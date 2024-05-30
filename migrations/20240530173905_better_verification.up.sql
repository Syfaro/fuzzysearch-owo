ALTER TABLE
    linked_account
ADD
    COLUMN created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT current_timestamp,
ADD
    COLUMN verification_key TEXT,
ADD
    COLUMN verified_at TIMESTAMP WITH TIME ZONE;

UPDATE
    linked_account
SET
    verified_at = current_timestamp
WHERE
    linked_account.data->'verification_key' IS NULL;

UPDATE
    linked_account
SET
    verification_key = linked_account.data->>'verification_key'
WHERE
    linked_account.data->'verification_key' IS NOT NULL;

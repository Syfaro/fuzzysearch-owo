INSERT INTO
    patreon_webhook_event (linked_account_id, data)
VALUES
    ($1, $2) RETURNING id;

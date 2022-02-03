INSERT INTO
    user_setting (owner_id, setting, value)
VALUES
    ($1, $2, $3) ON CONFLICT (owner_id, setting) DO
UPDATE
SET
    value = EXCLUDED.value;

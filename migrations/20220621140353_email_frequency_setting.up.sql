INSERT INTO
    user_setting (owner_id, setting, value)
SELECT
    owner_id,
    'email-frequency',
    to_jsonb (
        CASE
            WHEN value::boolean THEN 'instantly'
            ELSE 'never'
        END
    )
FROM
    user_setting
WHERE
    setting = 'email-notifications' ON CONFLICT DO NOTHING;

SELECT
    value
FROM
    user_setting
WHERE
    owner_id = $1
    AND setting = $2;

SELECT
    *
FROM
    user_account
WHERE
    id = $1
    AND api_token = $2;

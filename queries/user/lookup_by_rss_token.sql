SELECT
    *
FROM
    user_account
WHERE
    id = $1
    AND rss_token = $2;

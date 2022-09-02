SELECT
    *
FROM
    user_account
WHERE
    rss_token = $1;

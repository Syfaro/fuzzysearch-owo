SELECT
    owner_id,
    request_key,
    request_secret
FROM
    twitter_auth
WHERE
    request_key = $1;

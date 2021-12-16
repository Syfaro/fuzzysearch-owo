SELECT
    *
FROM
    linked_account
WHERE
    owner_id = $1
    AND source_site = $2
    AND data ->> 'site_id' = $3;

SELECT
    id,
    owner_id
FROM
    linked_account
WHERE
    source_site = $1
    AND lower(username) = lower($2);

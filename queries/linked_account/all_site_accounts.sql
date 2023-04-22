SELECT
    id
FROM
    linked_account
WHERE
    source_site = $1
    AND disabled = false;

SELECT
    *
FROM
    user_event
WHERE
    owner_id = $1
    AND (
        $4::text IS NULL
        OR event_name = $4
    )
    AND (
        $5::text IS NULL
        OR data->'SimilarImage'->>'site' = $5
    )
    AND (
        NOT $6::boolean
        OR (
            data->'SimilarImage'->>'site',
            lower(data->'SimilarImage'->>'posted_by')
        ) NOT IN (
            SELECT
                site,
                lower(site_username)
            FROM
                user_allowlist
            WHERE
                owner_id = $1
        )
    )
ORDER BY
    last_updated DESC
LIMIT
    $2 OFFSET ($3::integer * $2::integer);

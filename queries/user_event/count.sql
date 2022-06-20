SELECT
    count(*)
FROM
    user_event
WHERE
    owner_id = $1
    AND (
        $2::text IS NULL
        OR event_name = $2
    )
    AND (
        $3::text IS NULL
        OR data->'SimilarImage'->>'site' = $3
    )
    AND (
        NOT $4::boolean
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
    );

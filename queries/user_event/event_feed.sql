SELECT
    *
FROM
    user_event
WHERE
    owner_id = $1 AND
    $4::text IS NULL OR event_name = $4 AND
    $5::text IS NULL OR data->'SimilarImage'->>'site' = $5
ORDER BY
    last_updated DESC
LIMIT
    $2 OFFSET ($3::integer * $2::integer);

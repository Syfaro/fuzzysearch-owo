SELECT
    count(*)
FROM
    user_event
WHERE
    owner_id = $1 AND
    $2::text IS NULL OR event_name = $2 AND
    $3::text IS NULL OR data->'SimilarImage'->>'site' = $3;

WITH inserted AS (
    INSERT INTO
        bluesky_repo (did)
    SELECT
        $1
    WHERE
        NOT EXISTS (
            SELECT
                1
            FROM
                bluesky_repo
            WHERE
                did = $1
        ) ON CONFLICT (did) DO NOTHING RETURNING id
)
SELECT
    id AS "id!"
FROM
    inserted
UNION ALL
SELECT
    id
FROM
    bluesky_repo
WHERE
    did = $1
LIMIT
    1;

WITH searches AS (
    SELECT
        *
    FROM
        jsonb_to_recordset($2::jsonb) AS searches (site text, site_username text)
)
SELECT
    user_allowlist.*
FROM
    searches
    JOIN user_allowlist ON user_allowlist.owner_id = $1
    AND user_allowlist.site = searches.site
    AND lower(user_allowlist.site_username) = lower(searches.site_username)

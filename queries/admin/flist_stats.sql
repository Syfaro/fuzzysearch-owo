SELECT
    COALESCE(
        (
            SELECT
                count(*)
            FROM
                flist_file
            WHERE
                discovered_at IS NOT NULL
                AND discovered_at > current_timestamp - interval '24 hours'
        ),
        0
    ) "recent_posts!";

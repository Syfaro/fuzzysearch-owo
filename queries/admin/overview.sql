SELECT
    COALESCE(
        (
            SELECT
                count(*)
            FROM
                user_account
        ),
        0
    ) "total_users!",
    COALESCE(
        (
            SELECT
                count(*)
            FROM
                linked_account
        ),
        0
    ) "linked_accounts!",
    COALESCE(
        (
            SELECT
                count(DISTINCT user_id)
            FROM
                user_session
            WHERE
                last_used > current_timestamp - interval '24 hours'
        ),
        0
    ) "recent_users!",
    COALESCE(
        (
            SELECT
                sum(content_size)::bigint
            FROM
                owned_media_item
        ),
        0
    ) "total_filesize!",
    COALESCE(
        (
            SELECT
                count(*)
            FROM
                owned_media_item
            WHERE
                last_modified > current_timestamp - interval '24 hours'
        ),
        0
    ) "recent_media!";

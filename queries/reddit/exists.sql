SELECT
    exists(
        SELECT
            1
        FROM
            reddit_post
        WHERE
            fullname = $1
    ) "exists!";

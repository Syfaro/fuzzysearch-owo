SELECT
    coalesce(count(*), 0) "count!",
    coalesce(sum(content_size)::bigint, 0) "total_content_size!"
FROM
    owned_media_item
WHERE
    owner_id = $1;

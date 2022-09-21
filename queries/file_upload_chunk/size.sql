SELECT
    sum(size) total_size
FROM
    file_upload_chunk
WHERE
    owner_id = $1;

SELECT
    sequence_number
FROM
    file_upload_chunk
WHERE
    owner_id = $1
    AND collection_id = $2
ORDER BY
    sequence_number ASC;

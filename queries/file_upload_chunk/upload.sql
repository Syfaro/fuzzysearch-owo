INSERT INTO
    file_upload_chunk (owner_id, collection_id, size)
VALUES
    ($1, $2, $3) RETURNING sequence_number;

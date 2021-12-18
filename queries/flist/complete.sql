UPDATE
    flist_import_run
SET
    finished_at = current_timestamp,
    max_id = $2
WHERE
    id = $1;

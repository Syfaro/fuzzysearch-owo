INSERT INTO
    flist_import_run (starting_id)
VALUES
    ($1) RETURNING id;

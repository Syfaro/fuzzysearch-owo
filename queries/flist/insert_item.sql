INSERT INTO
    flist_file (id, ext, character_name)
VALUES
    ($1, $2, $3) ON CONFLICT DO NOTHING;

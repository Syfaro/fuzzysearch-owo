CREATE TABLE flist_file (
    id int PRIMARY KEY,
    ext text NOT NULL,
    character_name text NOT NULL,
    size int,
    sha256 bytea,
    perceptual_hash bigint
);

CREATE TABLE flist_import_run (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    started_at timestamp with time zone NOT NULL DEFAULT current_timestamp,
    finished_at timestamp with time zone,
    starting_id int NOT NULL,
    max_id int
);

CREATE INDEX flist_file_perceptual_hash_idx ON flist_file USING spgist (perceptual_hash bktree_ops);

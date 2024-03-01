ALTER TABLE
    flist_file
ADD
    COLUMN discovered_at timestamp with time zone;

ALTER TABLE
    flist_file
ALTER COLUMN
    discovered_at
SET
    DEFAULT current_timestamp;

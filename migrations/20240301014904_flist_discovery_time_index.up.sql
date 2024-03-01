CREATE INDEX flist_file_discovery_time_idx ON flist_file (discovered_at DESC)
WHERE
    (discovered_at IS NOT NULL);

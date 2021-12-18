UPDATE
    flist_file
SET
    sha256 = $2,
    size = $3,
    perceptual_hash = $4
WHERE
    id = $1;

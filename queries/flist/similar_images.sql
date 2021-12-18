SELECT
    *
FROM
    flist_file
WHERE
    perceptual_hash <@ ($1, $2);

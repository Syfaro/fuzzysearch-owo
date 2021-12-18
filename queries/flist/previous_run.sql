SELECT
    *
from
    flist_import_run
ORDER BY
    finished_at DESC NULLS FIRST
LIMIT
    1;

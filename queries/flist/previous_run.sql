SELECT
    *
from
    flist_import_run
ORDER BY
    finished_at DESC NULLS FIRST,
    started_at DESC
LIMIT
    1;

SELECT
    linked_account_id,
    linked_account.source_site,
    started_at,
    completed_at,
    expected_count,
    cardinality(loaded_ids) loaded_count
FROM
    linked_account_import
    JOIN linked_account ON linked_account.id = linked_account_import.linked_account_id
ORDER BY
    started_at DESC
LIMIT
    100;

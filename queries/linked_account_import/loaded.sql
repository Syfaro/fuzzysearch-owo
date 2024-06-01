UPDATE
    linked_account_import
SET
    loaded_ids = array_append(loaded_ids, $2)
WHERE
    linked_account_id = $1
RETURNING
    expected_count,
    cardinality(loaded_ids) loaded_count;

SELECT
  owned_media_item.*
FROM
  owned_media_item
  JOIN linked_account on owned_media_item.account_id = linked_account.id
WHERE
  linked_account.source_site = $1
  AND source_id = $2;

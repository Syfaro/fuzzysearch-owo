SELECT
  owned_media_item_accounts.id "id!",
  owned_media_item_accounts.owner_id "owner_id!",
  perceptual_hash,
  sha256_hash "sha256_hash!: Sha256Hash",
  last_modified "last_modified!",
  content_url,
  content_size,
  thumb_url,
  event_count "event_count!",
  last_event,
  accounts "accounts: sqlx::types::Json<Vec<OwnedMediaItemAccount>>"
FROM
  owned_media_item_accounts
  JOIN owned_media_item_account ON owned_media_item_accounts.id = owned_media_item_account.owned_media_item_id
  JOIN linked_account on owned_media_item_account.account_id = linked_account.id
WHERE
  owned_media_item_accounts.owner_id = $1
  AND linked_account.source_site = $2
  AND owned_media_item_account.source_id = $3;

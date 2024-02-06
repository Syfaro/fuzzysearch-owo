SELECT
    id "id!",
    owner_id "owner_id!",
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
    LEFT JOIN owned_media_item_account ON owned_media_item_accounts.id = owned_media_item_account.owned_media_item_id
WHERE
    owner_id = $1 AND
    $4::uuid IS NULL OR owned_media_item_account.account_id = $4
ORDER BY
    last_modified DESC
LIMIT
    $2 OFFSET ($3::integer * $2::integer);

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
WHERE
    owner_id = $1
    AND sha256_hash = $2;

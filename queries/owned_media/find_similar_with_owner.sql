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
    noisy_media,
    accounts "accounts: sqlx::types::Json<Vec<OwnedMediaItemAccount>>"
FROM
    owned_media_item_accounts
WHERE
    owner_id = $1
    AND perceptual_hash <@ ($2, $3);

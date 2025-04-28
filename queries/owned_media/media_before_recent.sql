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
    AND (
        $4::uuid IS NULL
        OR exists(
            SELECT
                1
            FROM
                owned_media_item_account
            WHERE
                owned_media_item_account.owned_media_item_id = owned_media_item_accounts.id
                AND account_id = $4
        )
    )
ORDER BY
    last_event DESC NULLS LAST,
    last_modified DESC
LIMIT
    $2 OFFSET ($3::integer * $2::integer);

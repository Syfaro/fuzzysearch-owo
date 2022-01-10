SELECT
  owned_media_item.id,
  owned_media_item.owner_id,
  owned_media_item.account_id,
  owned_media_item.source_id,
  owned_media_item.perceptual_hash,
  owned_media_item.sha256_hash "sha256_hash: Sha256Hash",
  owned_media_item.link,
  owned_media_item.title,
  owned_media_item.posted_at,
  owned_media_item.last_modified,
  owned_media_item.content_url,
  owned_media_item.content_size,
  owned_media_item.thumb_url
FROM
  owned_media_item
  JOIN linked_account on owned_media_item.account_id = linked_account.id
WHERE
  linked_account.source_site = $1
  AND source_id = $2;

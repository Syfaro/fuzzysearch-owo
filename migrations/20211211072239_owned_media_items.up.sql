CREATE TABLE owned_media_item (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id uuid NOT NULL REFERENCES user_account (id) ON DELETE CASCADE,
    account_id uuid REFERENCES linked_account (id) ON DELETE CASCADE,
    source_id text,
    perceptual_hash bigint,
    sha256_hash bytea NOT NULL,
    link text,
    title text,
    posted_at timestamp with time zone,
    last_modified timestamp with time zone NOT NULL,
    content_url text,
    content_size bigint,
    thumb_url text
);

CREATE UNIQUE INDEX owned_media_item_lookup ON owned_media_item (account_id, source_id);

CREATE INDEX owned_media_item_sha256_hash ON owned_media_item (sha256_hash);

CREATE INDEX owned_media_item_posted_at ON owned_media_item (posted_at);

CREATE EXTENSION IF NOT EXISTS bktree;

CREATE INDEX owned_media_perceptual_hash_idx ON owned_media_item USING spgist (perceptual_hash bktree_ops);

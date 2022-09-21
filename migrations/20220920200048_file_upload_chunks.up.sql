CREATE TABLE file_upload_chunk (
    owner_id uuid NOT NULL REFERENCES user_account (id) ON DELETE CASCADE,
    collection_id uuid NOT NULL,
    sequence_number bigserial NOT NULL,
    size integer NOT NULL,
    created_at timestamp with time zone NOT NULL DEFAULT current_timestamp,
    PRIMARY KEY (owner_id, collection_id, sequence_number)
);

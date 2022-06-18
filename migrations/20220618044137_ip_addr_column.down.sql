UPDATE
    user_session
SET
    source = source || jsonb_build_object('source_data', creation_ip);

ALTER TABLE
    user_session DROP COLUMN creation_ip;

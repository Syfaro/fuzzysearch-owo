ALTER TABLE
    user_session
ADD
    COLUMN creation_ip inet;

UPDATE
    user_session
SET
    creation_ip = cast(source->>'source_data' AS inet);

UPDATE
    user_session
SET
    source = source - 'source_data';

CREATE OR REPLACE FUNCTION base32_lower_decode(s text) RETURNS bytea
LANGUAGE plpgsql IMMUTABLE STRICT AS $$
DECLARE
    alphabet CONSTANT text := 'abcdefghijklmnopqrstuvwxyz234567';
    buffer bigint := 0; bits int := 0; result bytea := '\x'; i int; v int;
BEGIN
    FOR i in 1..length(s) LOOP
        v := strpos(alphabet, substr(s, i, 1)) - 1;
        IF v < 0 THEN RAISE EXCEPTION 'invalid base32 char "%"', substr(s, i, 1); END IF;
        buffer := (buffer << 5) | v;
        bits := bits + 5;
        IF bits >= 8 THEN
            bits := bits - 8;
            result := result || set_byte('\x00', 0, ((buffer >> bits) & 255)::int);
            buffer := buffer & ((1 << bits) - 1);
        END IF;
    END LOOP;
    RETURN result;
END;
$$;

ALTER TABLE bluesky_image ALTER COLUMN blob_cid TYPE bytea USING base32_lower_decode(substr(blob_cid, 2));

DROP FUNCTION base32_lower_decode(text);

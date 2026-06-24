CREATE OR REPLACE FUNCTION base32_lower_encode(b bytea) RETURNS text
LANGUAGE plpgsql IMMUTABLE STRICT AS $$
DECLARE
    alphabet CONSTANT text := 'abcdefghijklmnopqrstuvwxyz234567';
    buffer bigint := 0; bits int := 0; result text := ''; i int;
BEGIN
    FOR i IN 0..length(b)-1 LOOP
        buffer := (buffer << 8) | get_byte(b, i);
        bits := bits + 8;
        WHILE bits >= 5 LOOP
            bits := bits - 5;
            result := result || substr(alphabet, (((buffer >> bits) & 31)::int) + 1, 1);
            buffer := buffer & ((1 << bits) - 1);
        END LOOP;
    END LOOP;
    IF bits > 0 THEN
        result := result || substr(alphabet, (((buffer << (5 - bits)) & 31)::int) + 1, 1);
    END IF;
    RETURN result;
END;
$$;

ALTER TABLE bluesky_image ALTER COLUMN blob_cid TYPE text USING 'b' || base32_lower_encode(blob_cid);

DROP FUNCTION base32_lower_encode(bytea);

-- Fix: "column reference prekey_id is ambiguous" in consume_one_time_prekey()
-- The function's RETURNS TABLE(prekey_id) clashed with one_time_prekeys.prekey_id
-- Use table aliases to disambiguate all column references.

CREATE OR REPLACE FUNCTION consume_one_time_prekey(target_amid VARCHAR(64))
RETURNS TABLE(prekey_id INTEGER, public_key TEXT) AS $$
BEGIN
    RETURN QUERY
    UPDATE one_time_prekeys otp
    SET consumed = TRUE, consumed_at = NOW()
    WHERE otp.id = (
        SELECT sub.id FROM one_time_prekeys sub
        WHERE sub.amid = target_amid AND NOT sub.consumed
        ORDER BY sub.prekey_id
        LIMIT 1
        FOR UPDATE SKIP LOCKED
    )
    RETURNING otp.prekey_id, otp.public_key;
END;
$$ LANGUAGE plpgsql;

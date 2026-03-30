-- X3DH Prekey storage
-- Version: 0.2

-- Signed prekeys (medium-term, rotated every ~7 days)
CREATE TABLE signed_prekeys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    amid VARCHAR(64) NOT NULL REFERENCES agents(amid) ON DELETE CASCADE,
    prekey_id INTEGER NOT NULL,
    public_key TEXT NOT NULL,
    signature TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(amid, prekey_id)
);

-- One-time prekeys (consumed on each new session)
CREATE TABLE one_time_prekeys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    amid VARCHAR(64) NOT NULL REFERENCES agents(amid) ON DELETE CASCADE,
    prekey_id INTEGER NOT NULL,
    public_key TEXT NOT NULL,
    consumed BOOLEAN NOT NULL DEFAULT FALSE,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(amid, prekey_id)
);

-- Indexes for efficient lookups
CREATE INDEX idx_signed_prekeys_amid ON signed_prekeys(amid);
CREATE INDEX idx_one_time_prekeys_amid ON one_time_prekeys(amid);
CREATE INDEX idx_one_time_prekeys_available ON one_time_prekeys(amid, consumed) WHERE NOT consumed;

-- Function to get and consume one prekey atomically
CREATE OR REPLACE FUNCTION consume_one_time_prekey(target_amid VARCHAR(64))
RETURNS TABLE(prekey_id INTEGER, public_key TEXT) AS $$
BEGIN
    RETURN QUERY
    UPDATE one_time_prekeys
    SET consumed = TRUE, consumed_at = NOW()
    WHERE id = (
        SELECT id FROM one_time_prekeys
        WHERE amid = target_amid AND NOT consumed
        ORDER BY prekey_id
        LIMIT 1
        FOR UPDATE SKIP LOCKED
    )
    RETURNING one_time_prekeys.prekey_id, one_time_prekeys.public_key;
END;
$$ LANGUAGE plpgsql;

-- Cleanup function for old consumed prekeys
CREATE OR REPLACE FUNCTION cleanup_consumed_prekeys()
RETURNS INTEGER AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    DELETE FROM one_time_prekeys
    WHERE consumed AND consumed_at < NOW() - INTERVAL '7 days';
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

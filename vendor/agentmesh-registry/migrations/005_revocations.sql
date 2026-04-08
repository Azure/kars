-- Revocations table for certificate revocation list
CREATE TABLE IF NOT EXISTS revocations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    amid VARCHAR(255) NOT NULL UNIQUE,
    reason VARCHAR(50) NOT NULL,
    revoked_by VARCHAR(255) NOT NULL,
    revoked_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for revocation lookups
CREATE INDEX IF NOT EXISTS idx_revocations_amid ON revocations(amid);

-- Index for listing by date
CREATE INDEX IF NOT EXISTS idx_revocations_revoked_at ON revocations(revoked_at DESC);

-- Index for finding who revoked
CREATE INDEX IF NOT EXISTS idx_revocations_revoked_by ON revocations(revoked_by);

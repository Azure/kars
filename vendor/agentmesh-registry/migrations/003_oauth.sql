-- OAuth states for CSRF protection during OAuth flow
CREATE TABLE IF NOT EXISTS oauth_states (
    state VARCHAR(255) PRIMARY KEY,
    amid VARCHAR(255) NOT NULL,
    provider VARCHAR(50) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL
);

-- Index for cleanup of expired states
CREATE INDEX IF NOT EXISTS idx_oauth_states_expires_at ON oauth_states(expires_at);

-- Agent verifications from OAuth providers
CREATE TABLE IF NOT EXISTS agent_verifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    amid VARCHAR(255) NOT NULL,
    provider VARCHAR(50) NOT NULL,
    provider_id VARCHAR(255) NOT NULL,
    email VARCHAR(255),
    username VARCHAR(255),
    display_name VARCHAR(255),
    verified_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(amid, provider)
);

-- Index for looking up verifications by AMID
CREATE INDEX IF NOT EXISTS idx_agent_verifications_amid ON agent_verifications(amid);

-- Index for looking up by provider and provider_id
CREATE INDEX IF NOT EXISTS idx_agent_verifications_provider ON agent_verifications(provider, provider_id);

-- Cleanup function for expired OAuth states (run periodically)
CREATE OR REPLACE FUNCTION cleanup_expired_oauth_states()
RETURNS INTEGER AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    DELETE FROM oauth_states WHERE expires_at < NOW();
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

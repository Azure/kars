-- AgentMesh Registry Schema
-- Version: 0.1

-- Custom enum types
CREATE TYPE trust_tier AS ENUM ('anonymous', 'verified', 'organization');
CREATE TYPE presence_status AS ENUM ('online', 'away', 'offline', 'dnd');

-- Organizations table (for Tier 1.5)
CREATE TABLE organizations (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    domain VARCHAR(255) NOT NULL UNIQUE,
    verified BOOLEAN NOT NULL DEFAULT FALSE,
    root_certificate TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Agents table
CREATE TABLE agents (
    id UUID PRIMARY KEY,
    amid VARCHAR(64) NOT NULL UNIQUE,
    signing_public_key VARCHAR(128) NOT NULL,
    exchange_public_key VARCHAR(128) NOT NULL,
    tier trust_tier NOT NULL DEFAULT 'anonymous',
    display_name VARCHAR(255),
    organization_id UUID REFERENCES organizations(id),
    capabilities TEXT[] NOT NULL DEFAULT '{}',
    relay_endpoint VARCHAR(512) NOT NULL DEFAULT 'wss://relay.agentmesh.online/v1/connect',
    direct_endpoint VARCHAR(512),
    status presence_status NOT NULL DEFAULT 'offline',
    reputation_score REAL NOT NULL DEFAULT 0.5 CHECK (reputation_score >= 0 AND reputation_score <= 1),
    last_seen TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Reputation records
CREATE TABLE reputation_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    target_amid VARCHAR(64) NOT NULL REFERENCES agents(amid),
    from_amid VARCHAR(64) NOT NULL REFERENCES agents(amid),
    session_id UUID NOT NULL,
    score REAL NOT NULL CHECK (score >= 0 AND score <= 1),
    tags TEXT[],
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Session cache for KNOCK optimization
CREATE TABLE session_cache (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    initiator_amid VARCHAR(64) NOT NULL,
    receiver_amid VARCHAR(64) NOT NULL,
    intent_category VARCHAR(64) NOT NULL,
    session_key_encrypted TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(initiator_amid, receiver_amid, intent_category)
);

-- Indexes
CREATE INDEX idx_agents_amid ON agents(amid);
CREATE INDEX idx_agents_tier ON agents(tier);
CREATE INDEX idx_agents_status ON agents(status);
CREATE INDEX idx_agents_capabilities ON agents USING GIN(capabilities);
CREATE INDEX idx_agents_reputation ON agents(reputation_score DESC);
CREATE INDEX idx_agents_last_seen ON agents(last_seen DESC);

CREATE INDEX idx_reputation_target ON reputation_records(target_amid);
CREATE INDEX idx_reputation_from ON reputation_records(from_amid);

CREATE INDEX idx_session_cache_initiator ON session_cache(initiator_amid);
CREATE INDEX idx_session_cache_receiver ON session_cache(receiver_amid);
CREATE INDEX idx_session_cache_expires ON session_cache(expires_at);

-- Function to update reputation score with anti-gaming measures
CREATE OR REPLACE FUNCTION update_agent_reputation(target VARCHAR(64))
RETURNS VOID AS $$
DECLARE
    new_score REAL;
    completion_rate REAL;
    peer_avg REAL;
    age_factor REAL;
    tier_bonus REAL;
    agent_tier trust_tier;
    agent_created TIMESTAMPTZ;
    old_score REAL;
    ratings_count INTEGER;
BEGIN
    -- Get agent info
    SELECT tier, created_at, reputation_score INTO agent_tier, agent_created, old_score
    FROM agents WHERE amid = target;

    -- Calculate completion rate (placeholder - would need session tracking)
    completion_rate := 0.8;

    -- Calculate weighted peer feedback average with anti-gaming discounts
    -- Tier 2 (anonymous) ratings get 50% weight
    -- Mutual ratings within 24h get 80% discount
    -- Same IP ratings after first get 0% weight
    -- New account (<7 days) ratings get 25% weight
    SELECT COALESCE(
        SUM(
            rf.score * rf.weight *
            CASE WHEN rf.rater_tier = 'anonymous' THEN 0.5 ELSE 1.0 END *
            CASE WHEN EXISTS (
                SELECT 1 FROM reputation_feedback rf2
                WHERE rf2.target_amid = rf.rater_amid
                AND rf2.rater_amid = rf.target_amid
                AND rf2.created_at > rf.created_at - INTERVAL '24 hours'
                AND rf2.created_at < rf.created_at + INTERVAL '24 hours'
            ) THEN 0.2 ELSE 1.0 END
        ) / NULLIF(SUM(
            rf.weight *
            CASE WHEN rf.rater_tier = 'anonymous' THEN 0.5 ELSE 1.0 END *
            CASE WHEN EXISTS (
                SELECT 1 FROM reputation_feedback rf2
                WHERE rf2.target_amid = rf.rater_amid
                AND rf2.rater_amid = rf.target_amid
                AND rf2.created_at > rf.created_at - INTERVAL '24 hours'
                AND rf2.created_at < rf.created_at + INTERVAL '24 hours'
            ) THEN 0.2 ELSE 1.0 END
        ), 0),
        0.5
    ) INTO peer_avg
    FROM reputation_feedback rf
    WHERE rf.target_amid = target
    AND rf.created_at > NOW() - INTERVAL '30 days';

    -- Get ratings count for minimum threshold
    SELECT COUNT(*) INTO ratings_count
    FROM reputation_feedback
    WHERE target_amid = target;

    -- Calculate age factor (max 1.0 after 30 days)
    age_factor := LEAST(1.0, EXTRACT(EPOCH FROM (NOW() - agent_created)) / (30 * 24 * 3600));

    -- Tier bonus
    tier_bonus := CASE agent_tier
        WHEN 'organization' THEN 0.2
        WHEN 'verified' THEN 0.1
        ELSE 0.0
    END;

    -- Calculate new score
    -- If less than 5 ratings, keep default 0.5 with a blend
    IF ratings_count < 5 THEN
        new_score := 0.5 + (peer_avg - 0.5) * (ratings_count::real / 5.0);
    ELSE
        new_score := (0.3 * completion_rate) + (0.4 * peer_avg) + (0.1 * age_factor) + (0.2 * tier_bonus);
    END IF;

    -- Clamp to valid range
    new_score := GREATEST(0.0, LEAST(1.0, new_score));

    -- Detect rapid change (>0.2 in 24h) and add flag
    IF ABS(new_score - old_score) > 0.2 THEN
        INSERT INTO agent_flags (amid, flag, expires_at)
        VALUES (
            target,
            CASE WHEN new_score > old_score THEN 'rapid_reputation_increase' ELSE 'rapid_reputation_decrease' END,
            NOW() + INTERVAL '24 hours'
        );
    END IF;

    -- Update agent
    UPDATE agents SET reputation_score = new_score, updated_at = NOW()
    WHERE amid = target;
END;
$$ LANGUAGE plpgsql;

-- Function to submit a rating with anti-gaming weight calculation
CREATE OR REPLACE FUNCTION submit_reputation_rating(
    p_target_amid VARCHAR(64),
    p_rater_amid VARCHAR(64),
    p_rater_tier trust_tier,
    p_session_id UUID,
    p_score REAL,
    p_tags TEXT[],
    p_rater_ip_hash VARCHAR(64)
)
RETURNS VOID AS $$
DECLARE
    weight REAL := 1.0;
    rater_created TIMESTAMPTZ;
    same_ip_count INTEGER;
BEGIN
    -- Get rater account age
    SELECT created_at INTO rater_created
    FROM agents WHERE amid = p_rater_amid;

    -- New account rating limit (25% weight for <7 days old)
    IF rater_created IS NOT NULL AND rater_created > NOW() - INTERVAL '7 days' THEN
        weight := weight * 0.25;
    END IF;

    -- Same-IP rating limit (first per 24h at full weight, subsequent at 0%)
    IF p_rater_ip_hash IS NOT NULL THEN
        SELECT COUNT(*) INTO same_ip_count
        FROM reputation_feedback
        WHERE target_amid = p_target_amid
        AND rater_ip_hash = p_rater_ip_hash
        AND created_at > NOW() - INTERVAL '24 hours';

        IF same_ip_count > 0 THEN
            weight := 0.0;
        END IF;
    END IF;

    -- Insert rating
    INSERT INTO reputation_feedback (
        target_amid, rater_amid, rater_tier, session_id, score, tags, weight, rater_ip_hash
    ) VALUES (
        p_target_amid, p_rater_amid, p_rater_tier, p_session_id, p_score, p_tags, weight, p_rater_ip_hash
    );

    -- Update reputation score
    PERFORM update_agent_reputation(p_target_amid);
END;
$$ LANGUAGE plpgsql;

-- Function to get reputation status (for lookup response)
CREATE OR REPLACE FUNCTION get_reputation_status(p_amid VARCHAR(64))
RETURNS VARCHAR(16) AS $$
DECLARE
    ratings_count INTEGER;
BEGIN
    SELECT COUNT(*) INTO ratings_count
    FROM reputation_feedback
    WHERE target_amid = p_amid;

    IF ratings_count < 5 THEN
        RETURN 'unrated';
    ELSE
        RETURN 'rated';
    END IF;
END;
$$ LANGUAGE plpgsql;

-- Cleanup function for expired sessions
CREATE OR REPLACE FUNCTION cleanup_expired_sessions()
RETURNS INTEGER AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    DELETE FROM session_cache WHERE expires_at < NOW();
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

-- ============== Certificate Tables ==============

-- Agent certificates table
CREATE TABLE agent_certificates (
    amid VARCHAR(64) PRIMARY KEY REFERENCES agents(amid),
    certificate TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Certificate revocation list
CREATE TABLE certificate_revocations (
    serial_number VARCHAR(64) PRIMARY KEY,
    amid VARCHAR(64),
    revoked_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    reason VARCHAR(255)
);

CREATE INDEX idx_cert_revocations_amid ON certificate_revocations(amid);

-- Agent flags for anti-gaming and rapid change detection
CREATE TABLE agent_flags (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    amid VARCHAR(64) NOT NULL REFERENCES agents(amid),
    flag VARCHAR(64) NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ
);

CREATE INDEX idx_agent_flags_amid ON agent_flags(amid);
CREATE INDEX idx_agent_flags_active ON agent_flags(active) WHERE active = true;

-- Reputation feedback with extended metadata for anti-gaming
CREATE TABLE reputation_feedback (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    target_amid VARCHAR(64) NOT NULL,
    rater_amid VARCHAR(64) NOT NULL,
    rater_tier trust_tier NOT NULL,
    session_id UUID NOT NULL,
    score REAL NOT NULL CHECK (score >= 0 AND score <= 1),
    tags TEXT[],
    weight REAL NOT NULL DEFAULT 1.0,
    rater_ip_hash VARCHAR(64),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_reputation_feedback_target ON reputation_feedback(target_amid);
CREATE INDEX idx_reputation_feedback_rater ON reputation_feedback(rater_amid);
CREATE INDEX idx_reputation_feedback_session ON reputation_feedback(session_id);
CREATE INDEX idx_reputation_feedback_created ON reputation_feedback(created_at);

-- Reputation feedbacks table
CREATE TABLE IF NOT EXISTS reputation_feedbacks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    target_amid VARCHAR(255) NOT NULL,
    from_amid VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    score FLOAT NOT NULL CHECK (score >= 0 AND score <= 1),
    tags TEXT[] DEFAULT '{}',
    from_tier VARCHAR(50) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(from_amid, session_id)
);

-- Indexes for reputation feedbacks
CREATE INDEX IF NOT EXISTS idx_reputation_feedbacks_target ON reputation_feedbacks(target_amid);
CREATE INDEX IF NOT EXISTS idx_reputation_feedbacks_from ON reputation_feedbacks(from_amid);
CREATE INDEX IF NOT EXISTS idx_reputation_feedbacks_session ON reputation_feedbacks(session_id);
CREATE INDEX IF NOT EXISTS idx_reputation_feedbacks_created ON reputation_feedbacks(created_at);

-- Completed sessions table for completion rate calculation
CREATE TABLE IF NOT EXISTS completed_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id VARCHAR(255) NOT NULL UNIQUE,
    initiator_amid VARCHAR(255) NOT NULL,
    receiver_amid VARCHAR(255) NOT NULL,
    intent VARCHAR(255) NOT NULL,
    outcome VARCHAR(50) NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indexes for completed sessions
CREATE INDEX IF NOT EXISTS idx_completed_sessions_initiator ON completed_sessions(initiator_amid);
CREATE INDEX IF NOT EXISTS idx_completed_sessions_receiver ON completed_sessions(receiver_amid);
CREATE INDEX IF NOT EXISTS idx_completed_sessions_outcome ON completed_sessions(outcome);
CREATE INDEX IF NOT EXISTS idx_completed_sessions_intent ON completed_sessions(intent);

-- Ensure agents table has reputation_score column
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'agents' AND column_name = 'reputation_score'
    ) THEN
        ALTER TABLE agents ADD COLUMN reputation_score FLOAT DEFAULT 0.5;
    END IF;
END $$;

-- Create index on reputation score for leaderboard queries
CREATE INDEX IF NOT EXISTS idx_agents_reputation ON agents(reputation_score DESC);

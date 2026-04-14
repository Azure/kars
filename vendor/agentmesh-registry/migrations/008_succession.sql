-- 008: Identity succession for agent handoff
--
-- Adds Dormant presence status and succession_log table for tracking
-- identity succession (A → B) and reclamation (B → A) during handoff.

-- Add 'dormant' to presence_status enum (for handed-off agents).
ALTER TYPE presence_status ADD VALUE IF NOT EXISTS 'dormant';

-- Succession log — tracks identity succession and reclamation events.
-- Each row records either a succession (A→B) or reclamation (B→A).
-- Only one active succession per predecessor is allowed.
CREATE TABLE IF NOT EXISTS succession_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- The agent being replaced (predecessor in forward, departing in reclaim)
    predecessor_amid TEXT NOT NULL,
    predecessor_signing_key TEXT NOT NULL,

    -- The agent taking over (successor in forward, original in reclaim)
    successor_amid TEXT NOT NULL,
    successor_signing_key TEXT NOT NULL,

    -- 'succession' or 'reclamation'
    event_type TEXT NOT NULL CHECK (event_type IN ('succession', 'reclamation')),

    -- Ed25519 signature from predecessor (succession) or both (reclamation)
    predecessor_signature TEXT NOT NULL,
    -- Co-signature from successor (required for reclamation, NULL for succession)
    successor_signature TEXT,

    -- Reason for the event (e.g., 'handoff', 'handoff_return_to_local')
    reason TEXT NOT NULL DEFAULT 'handoff',

    -- SHA-256 hash of this event (for chain integrity)
    event_hash TEXT NOT NULL,
    -- For reclamation: reference to the original succession event_hash
    original_succession_ref TEXT,

    -- Whether this succession is currently active (redirect is in effect)
    active BOOLEAN NOT NULL DEFAULT TRUE,

    -- Reputation score at time of event (copied between agents)
    reputation_at_event REAL,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Fast lookup: find active succession for a given predecessor
CREATE INDEX IF NOT EXISTS idx_succession_predecessor_active
    ON succession_log (predecessor_amid, active)
    WHERE active = TRUE;

-- Fast lookup: find active succession for a given successor
CREATE INDEX IF NOT EXISTS idx_succession_successor_active
    ON succession_log (successor_amid, active)
    WHERE active = TRUE;

-- Ensure only one active succession per predecessor
CREATE UNIQUE INDEX IF NOT EXISTS idx_succession_one_active_per_predecessor
    ON succession_log (predecessor_amid)
    WHERE active = TRUE AND event_type = 'succession';

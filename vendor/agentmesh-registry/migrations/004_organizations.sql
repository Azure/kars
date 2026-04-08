-- Organizations table updates for Tier 1.5 registration
-- This migration adds columns that may be missing from the initial schema

-- Add admin_amid column if not exists
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'organizations' AND column_name = 'admin_amid'
    ) THEN
        ALTER TABLE organizations ADD COLUMN admin_amid VARCHAR(255);
    END IF;
END $$;

-- Add dns_challenge column if not exists
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'organizations' AND column_name = 'dns_challenge'
    ) THEN
        ALTER TABLE organizations ADD COLUMN dns_challenge VARCHAR(255);
    END IF;
END $$;

-- Add dns_verified column if not exists
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'organizations' AND column_name = 'dns_verified'
    ) THEN
        ALTER TABLE organizations ADD COLUMN dns_verified BOOLEAN NOT NULL DEFAULT FALSE;
    END IF;
END $$;

-- Add verified_at column if not exists
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'organizations' AND column_name = 'verified_at'
    ) THEN
        ALTER TABLE organizations ADD COLUMN verified_at TIMESTAMPTZ;
    END IF;
END $$;

-- Make root_certificate nullable if it was NOT NULL
DO $$
BEGIN
    ALTER TABLE organizations ALTER COLUMN root_certificate DROP NOT NULL;
EXCEPTION
    WHEN others THEN
        -- Column might already be nullable or not exist
        NULL;
END $$;

-- Add default for id column if not exists
DO $$
BEGIN
    ALTER TABLE organizations ALTER COLUMN id SET DEFAULT gen_random_uuid();
EXCEPTION
    WHEN others THEN
        NULL;
END $$;

-- Index for domain lookups
CREATE INDEX IF NOT EXISTS idx_organizations_domain ON organizations(domain);

-- Index for admin lookups (only if admin_amid exists now)
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'organizations' AND column_name = 'admin_amid'
    ) THEN
        CREATE INDEX IF NOT EXISTS idx_organizations_admin ON organizations(admin_amid);
    END IF;
END $$;

-- Add organization_id to agents table if not exists
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'agents' AND column_name = 'organization_id'
    ) THEN
        ALTER TABLE agents ADD COLUMN organization_id UUID REFERENCES organizations(id);
    END IF;
END $$;

-- Index for looking up agents by organization
CREATE INDEX IF NOT EXISTS idx_agents_organization ON agents(organization_id);

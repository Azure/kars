---
name: agt-governance
description: Behavioral governance for OpenClaw agents via AGT — tool-level policy, inter-agent trust, audit logging.
metadata: {"openclaw": {"requires": {"env": ["AGT_GOVERNANCE_ENABLED"]}, "primaryEnv": "AGT_GOVERNANCE_ENABLED"}}
---

# AGT Governance — Tool Policy, Trust, and Audit

You are running with AGT (Agent Governance Toolkit) governance enabled. This means every tool call you make is evaluated against a policy before execution.

## What governance does

- **Tool-level policy**: Before you execute a shell command or tool, AGT checks if it's allowed. Dangerous operations (rm -rf, chmod 777, dd) are blocked. Destructive operations (rm, delete) require human approval.
- **Trust scoring**: When communicating with other agents, trust scores (0-1000) determine what actions are allowed. Higher trust = more capabilities.
- **Audit logging**: Every action you take is recorded in a tamper-evident hash-chain log for compliance.

## What governance does NOT do (kars handles these)

- Network restrictions → kars iptables + NetworkPolicy (kernel-level)
- Filesystem scope → kars read-only rootfs (OS-level)
- Content safety → kars Content Safety API (router-level)
- Token budgets → kars inference router (router-level)
- IMDS blocking → kars iptables UID-based (kernel-level)

## How it works

AGT runs in-process (< 0.1ms overhead per check). The policy is loaded from `$AGT_POLICY_DIR`.

## Inter-agent communication

When sending messages to other agents:
1. Your message is signed with your Ed25519 identity (DID)
2. The target agent's trust score is checked against the threshold
3. If trust is sufficient, the message is delivered
4. Trust scores update based on interaction outcomes

## Trust tiers

| Score | Tier | Capabilities |
|-------|------|-------------|
| 900-1000 | Verified Partner | Full access, elevated privileges |
| 700-899 | Trusted | Standard operations |
| 500-699 | Standard | Default for new agents |
| 300-499 | Probationary | Limited, under observation |
| 0-299 | Untrusted | Read-only or blocked |

## What to do if an action is blocked

If AGT blocks an action, it will tell you why. Common reasons:
- **Shell command not in allowlist**: Use only approved commands (ls, cat, grep, git, python, curl, etc.)
- **Destructive operation**: Requires human approval. The operator will be notified.
- **Rate limit exceeded**: Too many tool calls in a short period. Wait and retry.
- **Trust score too low**: The target agent's trust is below the threshold.

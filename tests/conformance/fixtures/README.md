# Conformance corpus fixtures

Vendored test vectors and fixtures for protocol-invariant tests.
**Never** pulled as a git dependency — we copy-and-pin per plan §5.4.

| Subdirectory | Source | Pinned commit / version | Phase landing |
|---|---|---|---|
| `signal/` | libsignal + agentmesh vectors | TBD — populated in the PR that wires the first assertion (Phase 0 follow-up) | 0 |
| `isolation/` | our own policy profiles | N/A — regenerated from `policy-engine/` | 0 |
| `oauth/` | RFC 9700 + MCP 2026 conformance | TBD — Phase 1 | 1 |
| `mcp/` | AAIF MCP test corpus | TBD — Phase 1 | 1 |
| `a2a/` | A2A spec fixtures | TBD — Phase 1 | 1 |
| `ap2/` | AP2 fixtures | TBD — Phase 1 | 1 |
| `supply-chain/` | Sigstore vectors | TBD — Phase 3 | 3 |

## Rule

Every vendored fixture carries a `SOURCE.md` sibling file naming:

- Upstream URL + commit hash.
- License (must be permissive — copy retained only for Apache-2.0 /
  MIT / BSD / CC0 sources; anything else triggers a legal review).
- The conformance spec(s) that consume it.
- The date fetched.

Principle §0.2 #10: every consumed spec / vector is cited to its
upstream source.

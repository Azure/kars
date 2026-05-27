# Internal Red-Team — Findings Log

> Living log of internal red-team / adversarial-test exercises against Kars. Each entry records what was tested, what was found, and how it was closed. Findings that are still open carry an `OPEN` tag and link to a tracking issue.

This is **not** a coordinated-disclosure inbox. External researchers please use the procedure in [`SECURITY.md`](../../SECURITY.md).

---

## Cadence

- Every release cuts an internal red-team window of at least 1 week.
- Every "agent capability" addition (new runtime adapter, new tool surface, new channel) gets a focused exercise.
- Findings are tracked in this file with stable IDs of the form `RT-YYYY-NN`.

## Severity

- **Critical** — sandbox escape, token disclosure, cross-tenant data path, signed-payload forgery.
- **High** — privilege escalation inside sandbox, audit-chain bypass, persistent denial of service.
- **Medium** — single-tenant DoS, log-only information disclosure, governance bypass that another layer catches.
- **Low** — UX or hardening gaps without a clear exploitation path.

Each finding records: ID, date, surface, severity, summary, status, fix commit / PR.

---

## Findings

### Initial baseline

The earlier surface was exercised against [`security/stride.md`](../security/stride.md). Findings closed during that window are not enumerated here individually.

### Current surface

Today's capabilities have been exercised against the per-route threat model — CRD, KarsEval reconciler, KarsMemory reconciler, A2A gateway, runtime adapters, MCP reconciler, content-safety floor, leader election, conditions/SSA, requeue jitter, chaos tier, controller metrics, and runtime-CLI hotspots. Each carries two-reviewer sign-off.

### Open list

| ID | Date | Surface | Severity | Summary | Status |
|----|------|---------|----------|---------|--------|
| `RT-2026-01` | 2026-04-25 | Sandbox seccomp | Low | `kars-strict` allows `getpid` (necessary for libc); confirmed not exploitable | **Closed** — by-design |
| `RT-2026-02` | 2026-04-26 | Inference router | Low | Long header values caused 100ms latency spikes; bounded buffer added | **Closed** — see PR archive |
| `RT-2026-03` | 2026-04-28 | AgentMesh KNOCK | Medium | Anonymous peer (trust 0) could trigger registry lookups before policy check | **Closed** — by routing trust-evaluation before lookup; see vendored-patch audit |
| `RT-2026-04` | 2026-04-30 | A2A gateway | Low | Non-allowlisted scheme echoed in error response | **Closed** — sanitised |

### Accepted residual gaps

These items were caught and explicitly deferred (each is documented in [`security/stride.md`](../security/stride.md) §Residual risk):

- **Cosign-on-admission gating** — admission does not yet enforce a signed-image requirement.
- **Static TrustGraph projection** — live edge changes require a sandbox roll.
- **Per-cluster token budget** — only per-tenant exists.

---

## How to file an internal finding

1. Open a private issue under the `red-team` label.
2. Reference the stable ID (`RT-YYYY-NN`).
3. Include: trust boundary affected (T1–T4 from STRIDE doc), repro steps, observed vs expected, severity rationale, suggested mitigation.
4. CC the security-audit reviewer roster (internal only).
5. Once landed, add a row to the **Findings** table above with the closing PR.

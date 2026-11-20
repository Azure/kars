# Security Audit - Router operator-only endpoint tier

Date: 2026-11-19
Scope: `inference-router/src/operator_auth/` (new), `inference-router/src/main.rs`
router assembly and `admin_auth_middleware`, `inference-router/src/routes/egress.rs`,
`inference-router/src/routes/governance.rs`, `inference-router/src/routes/mod.rs`,
the two affected router integration tests, the Kind E2E harness
(`tests/e2e/run.sh`), and `docs/runtimes/CONTRACT.md`.
Gated paths: `inference-router/src/routes/egress.rs`,
`inference-router/src/routes/governance.rs`, `inference-router/src/routes/mod.rs`.

## Summary

Some inference-router endpoints change or reveal the sandbox's own security
controls: egress Learn-mode toggling, the learned and effective allowlist, the
AGT audit chain, dynamic rate limits, and the registry reputation record. They
were mounted in the same `protected` tier as the agent-facing APIs. That tier
admits loopback callers without a token and otherwise checks the admin bearer
token. Inside a sandbox pod, the agent container shares the router's network
namespace and can read the admin token, so neither check separates an operator
from the sandboxed workload. The agent could therefore weaken its own egress
controls.

This change moves those endpoints into a separate operator tier behind a new
`operator_auth_middleware`:

- **Same-pod callers** (loopback or any local pod address) are attributed to
  the Linux UID that owns the client TCP socket, read from `/proc/net/tcp` and
  `/proc/net/tcp6`. Only UID 0 and the router's own UID are admitted. Any other
  UID, including the agent (UID 1000), receives `403` even with a valid admin
  token.
- **Remote callers** keep the existing admin semantics: optional
  `ROUTER_ADMIN_ALLOW_IPS`, then `Authorization: Bearer <admin-token>`. The
  shared check is factored into `check_remote_admin` and reused by
  `admin_auth_middleware`, so behaviour on the existing protected tier does not
  change.

## T1: New capability / attack surface? (NO new capability)

- No new endpoint, CLI option, CRD field, network destination, permission,
  credential, or trust anchor. Existing endpoints are only restricted further.
- The middleware reads the kernel socket tables through procfs, which every
  Linux container can already read. It does not parse request content to make
  its decision and does not trust headers such as `X-Forwarded-For`.
- No custom cryptography. No new dependency.

## T2: Security-control change? (Operator endpoints restricted)

Moved to the operator tier (`operator_egress_routes`, `operator_agt_routes`):

| Endpoint | Method |
|---|---|
| `/egress/learn` | POST |
| `/egress/learned`, `/egress/learned/blocked` | GET |
| `/egress/learned/clear` | POST |
| `/egress/allowlist` | GET |
| `/agt/audit`, `/agt/audit/verify` | GET |
| `/agt/rate-limit` | GET, PUT |
| `/agt/reputation` | GET |

Attribution rules:

- A connection is same-pod when the peer IP is loopback or is bound to a local
  interface. For those, the middleware looks up the `ESTABLISHED` socket whose
  local endpoint is the peer and whose remote port is the router's listening
  port, normalizing IPv4-mapped IPv6 entries. All matching sockets must have
  the same owner.
- If the client socket belongs to the router process itself (for example a
  connection made through `/egress/fetch`), the request is refused. The router
  cannot be used to reach its own operator tier.
- Ambiguous matches, missing entries, and unreadable socket tables are
  refused. The middleware fails closed.

Endpoints deliberately left on the existing tier because the in-sandbox plugin
uses them: `/egress/fetch`, `/agt/evaluate`, `/agt/trust*`, `/agt/status`,
`/agt/signing-counter`, `/sandbox/*`, `PUT /admin/model`, and `/internal/*`.
They are unchanged by this PR.

## T3: Availability and compatibility (no supported caller is broken)

Supported operator paths and the UID that reaches the router:

| Path | Caller UID | Result |
|---|---|---|
| CLI `kubectl exec -c inference-router -- kars-inference-router probe …` | router UID (1001) | admitted |
| `kubectl port-forward` to the router port | 0 (kubelet/CRI side of the pod netns) | admitted |
| Docker mode `docker exec` into the router container | 0 | admitted |
| Remote caller with admin token (and allowed IP when configured) | n/a | admitted |
| Agent container (UID 1000), with or without the admin token | 1000 | `403` |

- Every tagged CLI release since 2026-03-25 already reaches these endpoints
  through `-c inference-router` or port-forward. The only CLI code that
  exec'd into the agent container for them existed between 2026-03-21 and
  2026-03-25, and no release was tagged in that window.
- No runtime plugin, skill, or shipped script calls these endpoints from the
  agent container. Handoff and bring-up flows that exec into the agent
  container do not use them.
- Behaviour change for a same-pod, tokenless call to the pod IP: previously
  `401`, now `403`. Only the sandboxed workload can make that call.
- Hosts without procfs (for example macOS developer runs of the router binary)
  deny local operator calls. Remote calls with the admin token are unaffected.
- In the non-root Docker fallback, where the router and agent share a UID,
  the agent still reaches these endpoints, exactly as before. This is a
  pre-existing limitation of that fallback and is not a regression.
- Same-pod detection binds a probe socket to the peer address. If a pod sets
  `net.ipv4.ip_nonlocal_bind=1`, every address is bindable, so remote callers
  are classified as same-pod, no matching local socket is found, and the call
  is refused. Remote operator access fails closed in that configuration;
  `probe` and port-forward, which use loopback, are unaffected.

Known follow-up hardening, not changed here: separate operator and agent
tokens; IPv6 coverage in the egress guard; DNS-based destination validation in
`/egress/fetch` Learn mode; policy validation for `PUT /admin/model`; and
moving the remaining CLI `inspect` and `egress blocked` reads onto `probe`.

## Verification

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test --package kars-inference-router` (macOS): library and all
  integration suites passed.
- Linux (`rust:1.95-bookworm` container, real procfs):
  `cargo test --package kars-inference-router --lib` passed 1053/1053.
  `operator_auth` tests passed 16/16 when run as root and again as UID 1000.
  `agt_governance_integration` passed 26/26, and `egress_blocked_endpoint`
  passed 8/8.
- Kind E2E, `test_operator_routes_reject_sandbox_workloads` in
  `tests/e2e/run.sh`: 6/6 assertions pass on the fixed router. Against the
  unfixed base router, all 6 assertions fail, which confirms the test detects
  the regression. The test checks the agent container receives `403` with and
  without the token, that Learn mode stays off, that the router-container
  `probe` path still works, and that agent-facing `/agt/status` still returns
  `200`.
- Docker-mode simulation, run with Podman because the validation host has no
  Docker Engine. The router was started as UID 1001 in a root container.
  A root `podman exec`, the equivalent of Docker mode's root `docker exec`,
  received `200` for `POST /egress/learn` and `GET /egress/allowlist`, and
  `probe /agt/audit` succeeded. UID 1000 received `403` with and without the
  token, `/agt/status` stayed `200`, and Learn mode stayed off. Docker Engine
  itself was not exercised.
- AKS staging was not run for this change.
- `ci/check-loc.sh`, `ci/no-stubs.sh`, `ci/no-custom-crypto.sh`,
  `ci/no-null-provider-prod.sh`, `ci/a2a-module-isolation.sh`, and
  `ci/check-copyright-headers.sh` pass against `main`.

## Verdict

Author review and the evidence above support restricting these endpoints to
operator callers without breaking supported CLI, port-forward, or Docker-mode
operator workflows. On 2026-11-19, sole maintainer @pallakatos explicitly
stated, "I can be the signoff for the security audit and there is no other
maintainer", and authorized recording the second sign-off under their GitHub
identity for this change. Copilot is the implementation author, not an
independent human reviewer. No approval given for another PR is reused. The
audit check is unchanged. This record does not itself merge or deploy the
change.

Signed-off-by: Copilot App <223556219+Copilot@users.noreply.github.com>
Signed-off-by: pallakatos <191481949+pallakatos@users.noreply.github.com>

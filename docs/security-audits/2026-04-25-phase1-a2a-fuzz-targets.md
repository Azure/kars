# Security Audit: `phase1/a2a-fuzz-targets`

**Capability:** adds two cargo-fuzz targets covering the A2A 1.0.0
inbound JWS parser surface.

## 1. Summary

- `fuzz_a2a_jws` — feeds attacker-controlled protected-header JSON +
  payload bytes into `a2a::build_signing_input`. Panic = remote
  pre-auth DoS at the gateway.
- `fuzz_a2a_base64url` — feeds attacker-controlled bytes into
  `a2a::base64url_decode`. Panic = remote pre-auth DoS.
- Wired into `inference-router/fuzz/Cargo.toml`. Run nightly per
  the existing fuzz README.

## 2. Threat model

The router-internal A2A endpoint (`:8445`, lands in
`phase1/a2a-1.0.0-routes-internal`) terminates TLS for traffic from
the dedicated gateway. Every byte of an inbound AgentCard reaches
these two functions before any allow-list check or signature
verification — so any panic here is exploitable as a pre-auth
DoS even when D6 surgical opt-in is correctly configured.

The fuzz targets exercise the same code paths the router will
invoke in `phase1/a2a-1.0.0-routes-internal`, so any bug found by
the corpus blocks that branch.

## 3. Tests

- Compile-only verified locally (cargo-fuzz needs nightly; not in
  CI gates today, run on demand).
- Both targets reference public API already covered by 33 existing
  unit tests; fuzz extends coverage of the rejection paths.

## 4. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>

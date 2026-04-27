/**
 * Sandbox isolation invariants — seccomp / Landlock / egress-guard.
 *
 * See internal Phase 1 plan §5.4. These are e2e-class tests:
 * they need a running Kind cluster with the AzureClaw Helm chart
 * installed so the real sandbox image and NetworkPolicy / seccomp
 * / Landlock surface can be exercised.
 *
 * Skipped locally unless CONFORMANCE_E2E=1; the compat suite's Kind
 * harness (Phase 1) supplies the `kubectl` proxy used here.
 *
 * Every negative case here maps to a documented hardening path:
 *   - seccomp: policy-engine/profiles/seccomp/azureclaw-strict.json
 *   - Landlock: sandbox-images/openclaw/entrypoint.sh landlock_policy()
 *   - egress-guard: iptables rules in init container (see
 *     sandbox-images/openclaw/Dockerfile + entrypoint.sh)
 */
import { describe, it } from "vitest";

const E2E = process.env.CONFORMANCE_E2E === "1";
const d = E2E ? describe : describe.skip;

d("seccomp — syscall denial", () => {
  it.todo("forbidden syscall (e.g. mount) → EPERM, not silent allow");
  it.todo("seccomp profile is azureclaw-strict by default on sandbox pods");
  it.todo("MAP auto-inject assigns seccomp to pods missing it (Phase 1)");
  it.todo("denied syscall is counted in audit-log (not just kernel log)");
});

d("Landlock — filesystem write discipline", () => {
  it.todo("agent (UID 1000) cannot write to /sandbox/node_modules/");
  it.todo("agent cannot write to /sandbox/plugin-source/");
  it.todo("agent cannot write to /opt/openclaw/");
  it.todo("agent CAN write to /tmp/ and its own /sandbox/.workdir/");
  it.todo("Landlock-denied open emits EACCES, not EPERM (distinguishes from seccomp)");
});

d("egress-guard — network boundary", () => {
  it.todo("UID 1000 direct egress to external IP is blocked");
  it.todo("UID 1000 localhost:8443 (router) is allowed");
  it.todo("UID 1000 DNS (53/udp) is allowed");
  it.todo("curl from UID 1000 through 127.0.0.1:8444 forward proxy succeeds");
  it.todo("UID 1001 (router) has unrestricted egress (authenticates via IMDS)");
});

d("Router as the only network path", () => {
  it.todo("Foundry API call from agent-side code goes via router (never direct)");
  it.todo("router enforces Content Safety on prompt before forwarding");
  it.todo("router emits audit event per inference request (receipt id)");
});

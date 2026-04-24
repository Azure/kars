# Security Audit: `phase1/a2a-cilium-vap-isolation`

**Capability:** ships the highest-priority A2A invariant from
ADR-0001 D2 + D3 — "the router data plane is never publicly
exposed; A2A goes through the dedicated gateway only."

## 1. Summary

1. **VAP `azureclaw-no-public-router-exposure`** — refuses any
   K8s object in a sandbox namespace (label
   `azureclaw.azure.com/isolated=strict`) that would publicly
   expose pods: `Service` (LoadBalancer/NodePort), `Ingress`,
   `HTTPRoute`/`TLSRoute`/`TCPRoute`, `NetworkPolicy.ingress.from.ipBlock`
   = 0.0.0.0/0 or ::/0. Failure policy `Fail`. No break-glass.
2. **CCNP `azureclaw-a2a-gateway-to-router`** — when Cilium is
   enabled, restricts ingress on the router's `:8445` listener
   to traffic from the `azureclaw-a2a-gateway` ServiceAccount in
   the `azureclaw-system` namespace. L7-aware: only `POST /a2a/*`
   and `GET /a2a/health`.
3. New `cilium.enabled: false` toggle in `values.yaml`.

## 2. Threat model delta

This is the **defining invariant** of the A2A architecture from
ADR-0001 — without it, the entire ingress threat-model collapses.

The VAP is failure-policy `Fail` (closed); a misconfigured
admission webhook can't accidentally allow public exposure. The
VAP also has no break-glass, deliberately: an operator who needs
to publicly expose something MUST disable the policy at the
cluster level (audit-logged) rather than annotate around it.

The CCNP is conditional on Cilium being available because vanilla
Azure CNI is still common; equivalent NetworkPolicy is emitted
per-sandbox by the controller (lands in
`phase1/a2a-controller-revocation`).

## 3. Spec sources

- VAP: <https://kubernetes.io/docs/reference/access-authn-authz/validating-admission-policy/>
- CCNP: <https://docs.cilium.io/en/stable/network/kubernetes/policy/#ciliumclusterwidenetworkpolicy>
- Cilium L7 HTTP rules:
  <https://docs.cilium.io/en/stable/security/policy/language/#layer-7-examples>

## 4. Tests

- `helm template` renders cleanly.
- Behaviour test belongs in the Phase 0 compat suite once a Kind
  cluster with Cilium is in CI; current scope is config-correctness.

## 5. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>

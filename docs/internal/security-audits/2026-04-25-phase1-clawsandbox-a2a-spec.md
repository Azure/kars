# Security Audit: `phase1/clawsandbox-a2a-spec`

**Capability:** extends `ClawSandbox.spec` with the A2A 1.0.0 inbound
exposure block per ADR-0001 D6. Schema-only (reconciler/admission
land separately).

## 1. Summary

- Adds `spec.a2a` (`Option<A2aIngressConfig>`) to `ClawSandboxSpec`.
- New types: `A2aIngressConfig`, `AllowedCaller`, `AdvertisedSkill`,
  `A2aRateLimit`.
- Defaults encode fail-closed posture: `enabled=false`,
  `minimumTrustScore=700`, `bodyCapBytes=1MiB`, `sessionMaxSeconds=60`,
  `allowStreaming=false`.
- Backward-compatible: existing CRs remain valid; the new field is
  `Option`.

## 2. Threat model delta

This is the surface that, when `enabled: true`, exposes a sandbox to
inbound A2A traffic via the (yet to be built) `azureclaw-a2a-gateway`.
Current branch is **schema-only** — no exposure happens until
`phase1/a2a-controller-revocation` lands the reconciler logic.

The schema design itself enforces the surgical-exposure principles
from ADR-0001 D6:

| Sub-point | Schema enforcement |
|-----------|--------------------|
| 1. opt-in | `Option<>` + `enabled: false` default |
| 2. allowedCallers required | empty list will be admission-rejected |
| 3. JWS thumbprint pin | `AllowedCaller.jws_thumbprint` mandatory |
| 4. expiresAt mandatory | `Option<String>` + admission CEL max 30d |
| 5. advertisedSkills allow-list | empty list rejected by admission |
| 6. min trust score | default 700 |
| 7. rate limit | `A2aRateLimit` block |
| 8. body cap | default 1 MiB, hard ceiling 4 MiB by CEL |
| 9. session length | default 60s, ceiling 600s by CEL |
| 10. streaming | default off |
| 11. revoke-now | `enabled: true → false` triggers tear-down |

## 3. Tests

- 125 controller tests pass (unchanged).
- New schema validates via existing kube-rs CustomResource derive.
- Behavior tests will land with the reconciler PR.

## 4. CEL admission rules (deferred)

Per implementation-plan §7 entry 12, all new CRD fields require CEL
`x-kubernetes-validations`. The reconciler/admission PR will add:

```cel
self.expiresAt != null && timestamp(self.expiresAt) - now() <= duration("720h")
self.allowedCallers.size() > 0
self.advertisedSkills.size() > 0
self.bodyCapBytes <= 4194304
self.sessionMaxSeconds <= 600
```

These are documented here so they don't get lost between branches.

## 5. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>

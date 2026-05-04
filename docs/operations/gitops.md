# GitOps mode for egress allowlists (S12.g)

This walkthrough covers the **sign-by-default** + **`--emit-manifest`**
GitOps workflow shipped in slice S12.g (the close-out for S12). It
assumes you have already followed
[`docs/internal/policy-canonical-format.md`](../policy-canonical-format.md) for
the v1 canonical egress allowlist format and that your cluster has a
`SignerPolicy` ConfigMap installed (S12.d).

## TL;DR

```bash
# Operator workstation (CI step or laptop).
azureclaw egress my-agent --enforce \
  --emit-manifest ./gitops/my-agent-allowlist.yaml

# What happened:
#   1. Canonical allowlist YAML built from live ClawSandbox.
#   2. Bytes pushed to ACR as an OCI artifact.
#   3. Cosign signature applied (default-on; --no-sign opts out).
#   4. ClawSandbox patch written to the file — no kubectl call.

# GitOps repo
git add gitops/my-agent-allowlist.yaml
git commit -m "egress: refresh allowlist for my-agent"
git push   # → Argo CD / Flux applies it; controller verifies; rolls out.
```

## Why default-on signing

S12.a–S12.f built signed-allowlist support behind an opt-in `--sign`
flag. In S12.g we flip the default: any time you run
`azureclaw egress` with `--enforce` or `--approve`, the CLI signs the
resulting allowlist and patches `spec.networkPolicy.allowlistRef`
without you asking. This closes the most common foot-gun reported in
the S12.f review — operators graduating their sandbox to enforcement
mode in CI and forgetting to add `--sign`, leaving production with an
inline-only allowlist that the controller will not treat as
authoritative once a `SignerPolicy` is installed.

To opt out:

```bash
azureclaw egress my-agent --enforce --no-sign
#   ⚠ --no-sign: the resulting allowlist will be unsigned. The
#   controller will emit AllowlistVerified=False/SignerPolicyMissing
#   and refuse the artifact in authoritative mode. Use only for local
#   dev.
```

`--no-sign` is incompatible with `--emit-manifest` — GitOps mode
**requires** signed artifacts (an unsigned artifact would fail
authoritative-mode verify on the cluster with no operator present to
retry, fail-closed under S12.e).

## Why emit-manifest

The original S12.c flow ended with `kubectl patch` against the live
cluster. That works for ad-hoc operator runs from a laptop, but it
breaks two common production patterns:

1. **GitOps clusters** where the source of truth is a git repo
   reconciled by Argo CD or Flux — direct `kubectl patch` drifts
   immediately and the next sync reverts it.
2. **Hands-off CI signers** where a build pipeline holds a Sigstore
   identity but has no kubeconfig credentials for the production
   cluster (least-privilege).

`--emit-manifest <path>` writes the patch as a YAML file so you can
commit it to your GitOps repo. The cluster sees the change only when
your GitOps controller picks up the next sync.

## Workflow diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│  Operator workstation / CI                                          │
│  ┌─────────────────────────────────────────────────────────────┐    │
│  │ azureclaw egress my-agent --enforce \                       │    │
│  │   --emit-manifest ./gitops/my-agent-allowlist.yaml          │    │
│  └──────────────┬───────────────────────┬──────────────────────┘    │
│                 │ (1) build + push      │ (2) write file            │
│                 ▼                       ▼                           │
│         ┌───────────────────┐   ┌───────────────────────┐           │
│         │  ACR (OCI artifact│   │  ./gitops/            │           │
│         │  + cosign sig)    │   │   my-agent-allowlist  │           │
│         │                   │   │      .yaml            │           │
│         └─────────┬─────────┘   └──────────┬────────────┘           │
└───────────────────┼────────────────────────┼────────────────────────┘
                    │                        │ (3) git commit + push
                    │                        ▼
                    │              ┌───────────────────────┐
                    │              │  GitOps repo          │
                    │              │  (Argo CD / Flux)     │
                    │              └──────────┬────────────┘
                    │                         │ (4) reconcile
                    │                         ▼
                    │              ┌───────────────────────┐
                    │              │  Production cluster   │
                    │              │  ClawSandbox patched  │
                    │              └──────────┬────────────┘
                    │                         │ (5) controller
                    │                         │     verifies digest
                    │                         │     + cosign sig
                    └─────────────────────────┘
```

## File layout

The emitted file is a complete, valid `ClawSandbox` resource with only
the relevant fields set. Example output:

```yaml
# azureclaw egress allowlist — digest=sha256:9f8e… signer=keyless:fulcio
# Generated by 'azureclaw egress … --emit-manifest'.
# Commit this file unchanged; your GitOps controller applies it.
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: my-agent
  namespace: azureclaw-my-agent
  annotations:
    azureclaw.io/applied-via-gitops: "true"
spec:
  networkPolicy:
    allowlistRef:
      registry: myacr.azurecr.io
      repository: policy/egress-allowlist/my-agent
      digest: sha256:9f8e…
      artifactType: application/vnd.azureclaw.egress-allowlist.v1+yaml
```

Determinism guarantees:

- Hand-rolled emitter (not `yaml`/`js-yaml`) → byte-stable across CLI
  versions and across Node minor releases.
- Fixed key order (`apiVersion` → `kind` → `metadata` → `spec`).
- LF line endings, single trailing newline, no trailing whitespace.
- No timestamps, no random IDs, no source-host metadata in the
  comment header.

This means `git diff` between two emit-manifest runs against the same
allowlist is **empty** — only the digest changes when the underlying
endpoints change.

The leading comment surfaces the artifact digest + signer identity
for human review during PR review (operators don't need to `oras
discover` to see who signed it).

The marker annotation `azureclaw.io/applied-via-gitops=true` is
written so cluster-side audit tools can distinguish GitOps-applied
allowlists from `kubectl patch`-applied ones.

## CI integration (GitHub Actions)

```yaml
- name: Promote egress allowlist
  env:
    SIGSTORE_ID_TOKEN: ${{ steps.oidc.outputs.token }}
    AZURECLAW_REGISTRY: myacr.azurecr.io
  run: |
    azureclaw egress my-agent --namespace prod --enforce \
      --emit-manifest gitops/clusters/prod/my-agent-allowlist.yaml \
      --force

- name: Open PR
  uses: peter-evans/create-pull-request@v6
  with:
    title: "egress: refresh allowlist for my-agent"
    branch: bot/egress-my-agent
    paths: gitops/clusters/prod/my-agent-allowlist.yaml
```

`--force` is required in CI because the file usually already exists
from a previous run; the CLI refuses to clobber without it. (Without
`--force` the second run errors with
`refusing to overwrite existing file …`.)

`SIGSTORE_ID_TOKEN` (or `OIDC_TOKEN`) is auto-detected and switches
the cosign signer into `identity-token` mode — no interactive Fulcio
flow needed in CI.

## Failure modes

| Symptom                                                  | Cause                                                                                      | Fix                                                                                          |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------- |
| `--emit-manifest cannot be combined with --no-sign`      | GitOps mode requires signed artifacts                                                      | Drop `--no-sign`                                                                             |
| `--emit-manifest requires --enforce or --approve`        | Need a signing context to derive the allowlist                                              | Add `--enforce` or `--approve <domain>`                                                      |
| `refusing to overwrite existing file …`                  | Target file already exists                                                                  | Add `--force` (typical in CI)                                                                |
| Controller emits `AllowlistVerified=False/SignerPolicyMissing` | No `SignerPolicy` ConfigMap on the cluster                                          | Install one (S12.d)                                                                          |
| Controller emits `AllowlistVerified=False/IdentityMismatch`    | Signer identity not allowlisted in `SignerPolicy`                                  | Update the `SignerPolicy` SAN/issuer allowlist                                               |

## Migrating from kagent

`azureclaw migrate from-kagent` now emits a one-line "Next step" hint
when the translated bundle includes an egress allowlist, pointing
operators directly at the GitOps `--emit-manifest` flow. See
[`docs/internal/policy-canonical-format.md`](../policy-canonical-format.md).

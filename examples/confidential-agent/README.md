# Confidential Agent — Kata VM isolation

`basic-agent`'s twin, but with **per-pod dedicated-kernel isolation**
via Kata Containers. A container escape inside the sandbox is trapped
inside a lightweight VM, not on the host kernel.

Use this when the agent processes sensitive data (regulated industries,
multi-tenant clusters, code-execution agents) and you want kernel-level
blast-radius containment on top of AzureClaw's seccomp/RO-rootfs/UID
controls.

## How it differs from `basic-agent`

A single field in the `ClawSandbox`:

```yaml
spec:
  sandbox:
    isolation: "confidential"   # vs. "enhanced" in basic-agent
```

That tells the controller to schedule the Pod with
`runtimeClassName: kata-cc` (or your cluster's confidential runtime
class), giving the sandbox:

- A **dedicated guest kernel** in a hardware-isolated VM
- All the host syscall surface gated through the VM exit boundary —
  Leaky Vessels (CVE-2024-21626 runc breakout), Probllama
  (CVE-2024-37032), and similar container-escape classes terminate
  inside the VM
- The full AzureClaw layer stack on top: seccomp, RO rootfs, non-root,
  egress-guard, NetworkPolicy, InferencePolicy enforcement

Token budget is bumped to 1M/day vs basic-agent's 500k — confidential
workloads tend to be larger.

## Prereqs (cluster-side)

Your AKS cluster needs the **Kata Confidential Containers** add-on. If
you provisioned with `azureclaw up`, this isn't enabled by default —
follow the AKS docs for
[Confidential Containers on AKS](https://learn.microsoft.com/azure/aks/confidential-containers-overview)
to add the runtime class.

## Deploy

```bash
kubectl apply -f examples/confidential-agent/clawsandbox.yaml
kubectl get clawsandbox confidential-assistant -n azureclaw-system -w
```

## Verify isolation

The Pod will land on a Kata-capable node:

```bash
kubectl get pod -n azureclaw-confidential-assistant \
  -o jsonpath='{.items[0].spec.runtimeClassName}{"\n"}'
# → kata-cc
```

A simple kernel-info probe inside the sandbox shows you're in a guest:

```bash
azureclaw connect confidential-assistant --shell -- \
  cat /proc/version
# → typically a Kata-shipped kernel, not the AKS host kernel
```

## Cleanup

```bash
kubectl delete -f examples/confidential-agent/clawsandbox.yaml
```

## See also

- [`examples/basic-agent`](../basic-agent/) — same agent without Kata
  (use that if your cluster doesn't have Confidential Containers)
- [`docs/security.md`](../../docs/security.md) — where Kata sits in the
  nine-layer model
- [`docs/blueprints/02-enterprise-self-hosted.md`](../../docs/blueprints/02-enterprise-self-hosted.md)
  — production blueprint that pairs naturally with confidential isolation

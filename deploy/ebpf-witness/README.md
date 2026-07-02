# Kars eBPF datapath-completeness witness (optional)

An **independent, kernel-level witness** that observes what Kars sandboxes
*actually* send on the network and cross-checks it against the egress
allowlist the controller *declared* for each sandbox.

It answers a provenance question the router alone cannot: *"Is the router's
declared egress allowlist a complete description of the sandbox's real
datapath, as seen by the kernel — not by the process being governed?"*

Powered by [Inspektor Gadget](https://www.inspektor-gadget.io/) (CNCF, eBPF).
It is **entirely optional and off by default** — a plain Kars cluster works
without it, and nothing in the core controller/router depends on it. When you
don't install it, you pay zero cost (no DaemonSet, no eBPF programs).

## Why kernel-level

Kars already enforces egress at two layers:

1. **L4 `NetworkPolicy`** (port-level, `0.0.0.0/0 except RFC1918`), and
2. the **router's forward-proxy** CONNECT allowlist (host-level, from
   `karssandbox-<name>-egress-allowlist`).

Both are *in-band* — they are part of the thing being governed. A datapath
witness is *out-of-band*: it attaches eBPF programs in the kernel and records
every DNS query and outbound TCP connect a sandbox pod makes, independently of
the router. Comparing **observed** (kernel) against **declared** (controller)
yields a completeness proof:

| Verdict | Meaning |
|---|---|
| `COMPLIANT` | **Strict** enforcement and every external host the kernel observed is in the declared allowlist. |
| `BEYOND-DECLARED` | **Strict** enforcement but the kernel observed egress to a host **not** in the declared allowlist. The router's proxy should have blocked the *connect*; a DNS-only observation means intent without a connect (still worth surfacing). A TCP connect to an undeclared host is a real finding. |
| `LEARN` / `UNCONSTRAINED` | The sandbox is in **learning mode** (`spec.networkPolicy.egressMode` != `Strict`, the default) **or** the declared allowlist is empty. Enforcement is not active, so reaching hosts beyond any baseline is expected — the witness records the observed set as the baseline you would promote into a `strict` allowlist rather than flagging it. |

**DNS = intent, TCP connect = actual datapath.** The witness reports both. The
router proxy remains the enforcement point; the witness only *attests*.

## Requirements

- A Linux kernel with **BTF** (`/sys/kernel/btf/vmlinux`) and eBPF enabled on
  every node that runs sandboxes. Verify with:
  ```bash
  kubectl get nodes -o name | while read n; do echo "$n"; done
  # on a node:  ls /sys/kernel/btf/vmlinux   (must exist)
  ```
  AKS Azure-Linux and Ubuntu node images ship BTF. `kind` (kernel ≥ 5.8 with
  BTF) works too — this witness was validated on `kind` (kernel 6.12, BTF present).
- The `kubectl gadget` client plugin (installed by `install.sh` if missing).
- Privilege: Inspektor Gadget runs a **privileged DaemonSet** in its own
  `gadget` namespace (it must load eBPF programs and read `/sys`). This is the
  one real cost of enabling the witness — review `deploy/ebpf-witness/` and your
  cluster's PodSecurity posture before installing.

## Install (gated, opt-in)

```bash
# Explicit opt-in — refuses to run without it, so it never installs silently.
KARS_EBPF_WITNESS=1 deploy/ebpf-witness/install.sh

# ...with a continuous (headless) witness that keeps recording in the background:
KARS_EBPF_WITNESS=1 deploy/ebpf-witness/install.sh --continuous
```

`install.sh`:
1. installs the `kubectl gadget` client if it isn't on `PATH`,
2. `kubectl gadget deploy` — the Inspektor Gadget DaemonSet + RBAC into the
   `gadget` namespace,
3. with `--continuous`, creates two **headless** gadget instances
   (`trace_dns`, `trace_tcp`) that run in the background and can be attached to
   at any time (`kubectl gadget list` / `kubectl gadget attach <id>`).

## Produce a witness verdict

```bash
# On-demand: capture a bounded window and cross-check every sandbox namespace.
deploy/ebpf-witness/witness-verify.sh                 # human table
deploy/ebpf-witness/witness-verify.sh --json          # machine-readable
deploy/ebpf-witness/witness-verify.sh --window 30     # longer capture
deploy/ebpf-witness/witness-verify.sh --namespace kars-foo,kars-bar
```

The verifier is **self-contained**: it works whether or not you installed the
continuous instances — it runs a bounded `trace_dns` + `trace_tcp` capture,
reads each sandbox's `karssandbox-<name>-egress-allowlist` ConfigMap, and emits
a per-sandbox verdict. Example JSON record:

```json
{
  "namespace": "kars-acme-run-123",
  "sandbox": "acme-run-123",
  "declared_hosts": ["api.github.com"],
  "observed_dns": ["api.github.com", "raw.githubusercontent.com"],
  "observed_connects": 4,
  "beyond_declared": ["raw.githubusercontent.com"],
  "unused_declared": [],
  "verdict": "BEYOND-DECLARED"
}
```

## Continuous witness + verdict ConfigMap (for the Bridge / any consumer)

`install.sh --continuous` also deploys a small **aggregator** (`deploy/ebpf-witness/aggregator/`)
that runs in the `gadget` namespace, attaches to the persistent headless
gadgets each cycle, cross-checks the declared allowlists, and **publishes the
verdict to the `kars-datapath-witness` ConfigMap in `kars-system`** every ~30s:

```bash
kubectl -n kars-system get cm kars-datapath-witness -o jsonpath='{.data.witness\.json}' | jq .
```

```json
{
  "generated_at": "2026-07-02T13:51:32Z",
  "window_seconds": 15,
  "gadget": "inspektor-gadget",
  "sandboxes": [
    { "namespace": "kars-acme-run-123", "sandbox": "acme-run-123",
      "declared_hosts": ["api.github.com"],
      "observed_dns": ["api.github.com", "example.com"],
      "observed_connects": 4, "beyond_declared": ["example.com"],
      "unused_declared": [], "verdict": "BEYOND-DECLARED" }
  ]
}
```

This ConfigMap is the **decoupled integration surface** — a reader needs no
eBPF/gadget dependency, only permission to read one ConfigMap. On a plain Kars
cluster with no Bridge, `kubectl get cm kars-datapath-witness` is the whole API.

The aggregator uses a dedicated least-privilege ServiceAccount
(`kars-witness-aggregator`): `apps/daemonsets:list` + `pods:list` +
`pods/portforward:create` (what `kubectl-gadget` needs to reach the gadget
pods), `configmaps:get,list` cluster-wide (declared allowlists), and
`configmaps:write` **only** on `kars-datapath-witness` in `kars-system`.

## Consume the verdict

- **Kars Bridge** — the Operator Console **Datapath witness** page reads the
  ConfigMap via `GET /api/operator/datapath-witness` and renders per-sandbox
  verdicts live. When the witness isn't installed it shows enable instructions,
  never fabricated data.
- **Audit / receipts** — attach the verdict as a datapath-completeness claim
  next to the signed run receipt.
- **Alerting** — a `BEYOND-DECLARED` with a real TCP connect to an undeclared
  host is an egress-escape signal; forward to your SIEM.
- **Learn → strict promotion** — a `LEARN` verdict's `observed_dns` is exactly
  the allowlist you would promote a learn-mode sandbox into.

Because everything reads only core Kars objects (`KarsSandbox` + the
egress-allowlist ConfigMap the controller already publishes), the witness is a
standalone Kars artifact: it runs on any Kars cluster with **no Kars-Bridge
installed**. The `witness-verify.sh` on-demand verifier remains available for a
one-shot table/JSON without the continuous aggregator.

## Uninstall

```bash
deploy/ebpf-witness/uninstall.sh   # removes the aggregator, headless instances + the IG DaemonSet
```

## Cost / safety notes

- Zero cost when not installed. When installed: one privileged DaemonSet pod per
  node + the eBPF programs the active gadgets attach (tracepoints/kprobes for DNS
  and TCP connect — low overhead, per-event ring-buffer).
- The witness is **read-only**: it never blocks, drops, or modifies traffic. It
  cannot be a datapath outage source. Enforcement stays with the router proxy
  and `NetworkPolicy`.
- If your kernel lacks BTF, `install.sh` stops with a clear message rather than
  installing a DaemonSet that would `CrashLoopBackOff`.

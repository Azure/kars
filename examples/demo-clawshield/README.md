# Operation Claw Shield — multi-tenant attack-simulation demo

A 30-minute scripted demo showing **three tenants on one cluster**
(Contoso Bank, Fabrikam Legal, Northwind Traders), one of which gets
compromised via a poisoned legal document, and kars's nine
security layers containing the attack while the other two tenants
keep working.

The full timed walkthrough — phases, talking points, expected output,
threat-model table mapping each phase to a real-world CVE / paper —
lives in [`docs/internal/DEMO.md`](../../docs/internal/DEMO.md).

> **Picking between this and `lethal-trifecta-demo`?** This one
> showcases **multi-tenant isolation** (cross-namespace, runc/Kata
> escape, IMDS theft, lateral movement). The
> [`lethal-trifecta-demo`](../lethal-trifecta-demo/) showcases the
> **inference data path** (prompt injection, ToolPolicy method
> allowlist, ClawIdentity bearer-stripping, token budget). They're
> complementary — the lethal-trifecta demo is the recommended
> launch-day demo because it's anchored on a named, recent CVE; this
> one is the deeper "everything we ship" walkthrough.

## What's in this directory

| File | What it is |
|---|---|
| [`contoso-bank-agent.yaml`](contoso-bank-agent.yaml) | Tenant 1 — financial compliance agent (`enhanced` isolation) |
| [`fabrikam-legal-agent.yaml`](fabrikam-legal-agent.yaml) | Tenant 2 — **the victim**, processes the poisoned doc (`confidential` Kata isolation) |
| [`northwind-trade-agent.yaml`](northwind-trade-agent.yaml) | Tenant 3 — trading-desk agent (`enhanced` isolation) |
| [`poisoned-document.md`](poisoned-document.md) | The injection payload Fabrikam ingests |
| [`attack-simulation.sh`](attack-simulation.sh) | Runs the full attack chain inside the compromised sandbox; every step **must** be blocked |
| [`normal-workflow.sh`](normal-workflow.sh) | The benign three-tenant collaboration the attack interrupts |

## Layers each phase exercises

| Phase | Attack | Layer that catches it |
|---|---|---|
| 1. Setup | All three tenants run benign workflow | Baseline — everything green |
| 2. Injection | Poisoned doc loaded into Fabrikam | Content Safety prompt-shield (Foundry DefaultV2) |
| 3a. Exfiltration | Compromised agent posts to external C2 | NetworkPolicy default-deny + URL+method allowlist |
| 3b. Container escape | runc breakout (Leaky Vessels CVE-2024-21626 style) | Kata VM boundary (Fabrikam ships `confidential` isolation) |
| 4. Lateral movement | Probe Contoso's pod | Cross-namespace NetworkPolicy isolation |
| 5. Privilege escalation | Try to mount host fs / install miner | seccomp `kars-strict` + RO rootfs + non-root |
| 6. IMDS token theft | Reach 169.254.169.254 | egress-guard iptables (UID 1000 cannot reach IMDS) |

## Quick run

```bash
# Prereq: kars cluster with the Kata Confidential Containers add-on
# (Fabrikam uses `isolation: confidential`)
kubectl apply -f examples/demo-clawshield/

# Wait for all three sandboxes to go Ready
kubectl get karssandbox -A -w

# Demonstrate the benign multi-tenant workflow
bash examples/demo-clawshield/normal-workflow.sh

# Run the attack simulation inside the Fabrikam sandbox
kars connect fabrikam-legal-agent --shell
# inside the sandbox:
bash /sandbox/attack-simulation.sh
```

`attack-simulation.sh` ends with a pass/fail tally — every attack must
land in the BLOCKED column.

## Cleanup

```bash
kubectl delete -f examples/demo-clawshield/
```

## See also

- [`docs/internal/DEMO.md`](../../docs/internal/DEMO.md) — full timed
  walkthrough with talking points + threat-model citations
- [`examples/lethal-trifecta-demo`](../lethal-trifecta-demo/) — focused
  prompt-injection / inference-data-path demo (recommended launch demo)
- [`docs/security.md`](../../docs/security.md) — the nine-layer security
  model these phases exercise

